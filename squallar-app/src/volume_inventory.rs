//! The decoded volumes the app holds, behind the questions asked of them
//! rather than behind the maps that answer.
//!
//! Two stores, and they are **not** the same volume:
//!
//! * **The still** — what a pane's static (non-loop) render draws. Keyed
//!   `(site, collected-at)`, because two panes on one site are allowed to be
//!   parked at two different moments and each must draw its own. The moment is
//!   the pane's own `scan_info.timestamp` — the volume's first radial, by
//!   [`squallar_radar::types::volume_collected_at`] — so the key a reader asks
//!   with is the key the writer installed under. **That clock is the volume's
//!   identity, and it is not the loop download cache's key.** That cache is
//!   keyed by an *address* — the S3 key's second, which the archive fetches
//!   hand on as an arrival's `timestamp` — and the two are equal on 0 of the
//!   171 local Archive II volumes. A pane's own clock may be either one
//!   depending on where its `scan_info` came from, so
//!   `App::evict_unneeded_loop_scans` matches a parked volume against both;
//!   it once compared identity to address alone and swept every
//!   archive-fetched parked volume out from under its pane.
//! * **The base** — the most recent *complete* volume for a site, with the
//!   time its first radial was collected. It is the base of the current merged
//!   volume ([`squallar_radar::current::resolve`]) that sections, the 3D view
//!   and every other whole-volume reader stand on. It is keyed by site alone
//!   and deliberately so: "the site's newest whole volume" is a question about
//!   a site, and [`base_advances_to`](VolumeInventory::base_advances_to) is
//!   the monotone-forward rule that makes it one.
//!
//! The loop's own cache is keyed `(site, timestamp)` too and is a different
//! subsystem; it is not held here and this module deliberately knows nothing
//! about it.
//!
//! Every entry is a whole decoded volume — a **measured** 48.9 MiB median and
//! 74.6 MiB maximum (see [`MAX_RESIDENT_STILL_VOLUMES`]) across thousands of
//! per-radial buffers — so eviction hands the values back **owned**, for the
//! caller to pass to the deferred-drop path rather than free on the frame
//! thread.
//!
//! # Why residency is now a policy and not an accident
//!
//! Site-keyed, the still store was self-limiting: a second volume for a site
//! *replaced* the first. Keyed by moment it is not, so residency is held down
//! by two named mechanisms, both here:
//!
//! 1. [`retain_still`](VolumeInventory::retain_still) — every frame, an entry
//!    no pane names is dropped. This is the policy; it is tied to what panes
//!    actually reference and it can never drop a volume in use.
//! 2. [`MAX_RESIDENT_STILL_VOLUMES`] — a hard cap enforced at install, so a
//!    frame that lands several archive arrivals cannot grow the store without
//!    bound before (1) runs later in the same frame.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::NaiveDateTime;
use nexrad_model::data::Scan;
use squallar_device_profile::budget::MAX_PANES_DESKTOP;
use squallar_radar::nyquist::DeclaredNyquist;

/// A decoded volume and what its cuts declared their Nyquist velocity to be.
pub(crate) type Still = (Arc<Scan>, Arc<DeclaredNyquist>);

/// A [`Still`] plus the time the volume's first radial was collected.
pub(crate) type Base = (Arc<Scan>, Arc<DeclaredNyquist>, NaiveDateTime);

/// Which store an entry came from, as a **total order** — the tie-break
/// [`VolumeInventory::resident_scan_bytes`] uses to name one of two equal
/// pointers as the payer.
///
/// The variants' order is arbitrary and only has to be stable; what matters
/// is that a key built from it cannot collide across two stores, which a
/// `(site, Option<when>)` key alone could not because a base and a latest are
/// both site-keyed.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Store {
    Base,
    Latest,
    Still,
}

/// One priced volume as [`VolumeInventory::priced_volumes`] yields it: the
/// entry's ordering key, the allocation behind it, and what that allocation
/// was priced at when it arrived.
type PricedVolume<'a> = ((&'a str, Store, Option<NaiveDateTime>), *const Scan, usize);

/// The per-site latest cache as [`VolumeInventory::resident_scan_bytes_with`]
/// takes it: a site, the volume it is holding, and what that volume was
/// priced at where it arrived.
///
/// Priced by the caller and not walked here: `scan_bytes` is O(radials) and
/// this figure rides the telemetry tick — the same rule every other holder
/// in this file follows.
pub(crate) type LatestVolume<'a> = (&'a str, &'a Arc<Scan>, usize);

/// The most decoded still volumes held at once, across every site.
///
/// # This number is MEASURED. Do not adjust it by feel.
///
/// **The per-volume cost.** `squallar_radar::scan::decode_bytes` — the app's
/// real decode path — was run under a counting global allocator over **208
/// real archive volumes**: 108 from 9 sites (KABX KAKQ KBMX KBOX KDYX KEWX
/// KGLD KGRB KHGX) and 100 holdout from 10 more (incl. KIWX KMLB KSRX), across
/// three days chosen for regime spread (2025-04-23 and 2025-06-11 convective,
/// 2025-12-03 clear-air), VCPs 12/31/34/35/212/215, 1–21 sweeps, 360–11 160
/// radials, all Level II moments the volume carries. The figure is live heap
/// bytes held by the `DecodedScan` — `Scan` plus `DeclaredNyquist`:
///
/// | over all 208 | live heap held |
/// |---|---|
/// | median | 48.88 MiB |
/// | max | 74.63 MiB |
///
/// **Take 74.63 MiB as one slot.**
///
/// ## The anchors this replaces cannot be reproduced, and that is the point
///
/// Until 2026-09-04 this doc pinned 46.8 / 46.1 MiB medians and **58.3 MiB as
/// a worst case**, from the same 208 volumes and the same decode path. Those
/// six anchors are **superseded**: re-running the instrument that produced
/// them will reproduce them exactly and they will still be wrong, because the
/// defect was in the measurement window, not in the corpus or the code under
/// it.
///
/// That instrument took its baseline **after the compressed archive buffer had
/// been read**, and `decode_bytes` frees that buffer *inside* the window. So
/// every volume's recorded price was reduced by the size of its own compressed
/// archive — 0.34–17.96 MiB over this corpus. The prose said "with the archive
/// buffer already freed"; the arithmetic had subtracted it.
///
/// The correction is a second baseline taken after the read. It **reproduces
/// the old instrument** rather than merely disagreeing with it: the old
/// arbitration max to within 0.04 MiB, the old holdout max to 0.05, the old
/// minimum to 0.02, the old "31.7 MiB" floor landing exactly on the
/// second-smallest whole volume, and the per-row identity
/// `live − old_figure − archive_buffer = 0` closing on all 208 rows, over two
/// runs that were byte-identical. A future reader who finds 58.3 quoted
/// somewhere is looking at that subtraction, not at a second opinion.
///
/// **58.3 MiB was never a worst case.** 61 of the 208 volumes exceed it —
/// 29.3 % — which makes it the **70.7th percentile presented as a maximum**.
/// The corrected slot is 74.63 ÷ 58.3 = **1.28×** the one this file pinned,
/// and every total derived from it below moved with it.
///
/// The corrected median is **not** the old median plus the median archive
/// buffer, and no arithmetic here should be read as if it were: decoded size
/// is quantised by scan structure into 75 distinct values across the 208,
/// while the archive buffer is continuous, so the two distributions have
/// different shapes and their medians are not related by subtraction. The
/// maximum is the figure that *can* be reasoned about directly, because
/// maxima compose along a single row: 74.63 − 58.34 (the old maximum
/// unrounded) = 16.29 MiB, under the 17.96 MiB largest archive in the corpus,
/// with no assumption at all.
///
/// The re-run reported a median, a maximum and that exceedance count. It did
/// **not** restate a minimum, so the old 31.7 MiB floor and the 3.1 MiB
/// single-sweep truncated reading are recorded above as *superseded*, not as
/// replaced: this file currently pins no corrected lower bound, and one should
/// not be inferred from the numbers that remain.
///
/// ## What is still excluded from 74.63 MiB
///
/// Both instruments count **requested** sizes, so neither carries the
/// allocator's own per-block overhead: on the largest volume, at roughly
/// 56 000 live blocks, a further ~0.4–0.9 MiB. Named as excluded rather than
/// quietly absorbed — 74.63 is a floor on the real resident cost, not a
/// ceiling on it.
///
/// ## Corpus caveats, which travel with any percentile quoted from this set
///
/// Only the 06Z and 18Z hours were fetched — two instants a day, not a diurnal
/// population — and one of four planned seasonal dates silently failed to
/// fetch, so that season is *missing*, not sampled. Neither touches the window
/// defect; both bound how far "29.3 % exceed 58.3 MiB" generalises beyond
/// these 208.
///
/// **The floor is forced, not chosen.** Each of at most
/// [`MAX_PANES_DESKTOP`] panes may be parked at its own moment, so six slots
/// is the smallest cap that does not break the feature this key exists for:
/// 6 × 74.63 MiB = **448 MiB** worst case (was 6 × 58.3 = 350 MiB),
/// 6 × 48.88 MiB = 293 MiB typical. That is the same worst case the
/// site-keyed store already had, since six panes on six sites held six volumes
/// then too — re-keying moved *which* arrangements reach the bound, not the
/// bound.
///
/// **The two spare slots are the priced part.** The archive drain
/// (`poll_scan_results`) is the one installer that can land several volumes in
/// a single frame, and an arrival for a site whose pane has since switched
/// away is nobody's. Two slots of headroom buy that, at a measured
/// 2 × 74.63 MiB = **149 MiB** worst case (was 117 MiB), and are what stop
/// such an arrival displacing a volume a pane is showing before
/// [`retain_still`] runs at the end of the same frame. Total:
/// 8 × 74.63 MiB = **597 MiB** worst case (was 466 MiB),
/// 8 × 48.88 MiB = 391 MiB typical.
///
/// **The cap itself did not move**, and the correction gave no reason to move
/// it: it counts volumes, and what was mispriced is what one volume costs. Six
/// is still forced by [`MAX_PANES_DESKTOP`] and the two spare slots still buy
/// the same one-frame burst. What changed is that the residency this cap
/// admits is 131 MiB larger than this file used to claim.
///
/// Consumers outside this crate sized themselves against the superseded
/// 58.3 MiB — a reserve rounded up from it is no longer above the sample
/// maximum. Each has to re-derive from 74.63 in its own crate; none of them is
/// corrected by this doc.
///
/// [`retain_still`]: VolumeInventory::retain_still
pub(crate) const MAX_RESIDENT_STILL_VOLUMES: usize = MAX_PANES_DESKTOP + 2;

/// One resident still and when it was installed, so the cap has an oldest to
/// name. The counter is the store's own, not a clock: a wall clock on wasm is
/// coarse enough for two installs in a frame to tie.
struct StillEntry {
    volume: Still,
    installed: u64,
    /// The volume's host bytes by [`squallar_radar::scan_size::scan_bytes`],
    /// priced once here at install.
    ///
    /// Carried with the entry rather than re-derived, because the reading is
    /// wanted every telemetry tick and the price is a walk of every radial:
    /// summing eight stored `usize`s is what a tick can afford, and eight
    /// walks is not.
    bytes: usize,
}

/// The decoded volumes held, and the one owner of both stores.
#[derive(Default)]
pub(crate) struct VolumeInventory {
    /// The volume each pane's static render draws from, by site and by the
    /// moment the volume was collected. Nested rather than tuple-keyed so a
    /// per-frame read costs no `String` allocation.
    still: HashMap<String, HashMap<NaiveDateTime, StillEntry>>,
    /// Installs so far, the sequence [`MAX_RESIDENT_STILL_VOLUMES`] evicts by.
    installs: u64,
    /// The most recent complete volume for each site, with its collection time.
    base: HashMap<String, Base>,
    /// Each merge base's host bytes, keyed as [`base`](Self::base) is.
    ///
    /// Beside the store rather than inside it because [`Base`] is a tuple
    /// several modules destructure; a fourth element would ripple through
    /// every one of them for a figure only this file maintains. Every
    /// mutation of `base` in this file is a mutation of this map, and the two
    /// are pinned together by `the_inventorys_byte_total_tracks_both_stores`.
    base_bytes: HashMap<String, usize>,
    /// **The per-site latest cache's prices**, keyed as `App::latest_cached_scans`
    /// is — the volumes themselves live on `App`, because
    /// `App::handle_jump_to_live` moves one out of that cache and into the
    /// still store, and the store lives beside the panes.
    ///
    /// A row follows the cache at all four of its seams: written where an
    /// entry is filed (`price_latest`, the archive drain and the chunk
    /// landing), pruned where entries are evicted (`forget_latest`) and where
    /// one is moved out into the still store (`forget_latest_site`). The
    /// reader, `resident_scan_bytes_with`, walks the live cache and looks each
    /// entry's price up here, so a row alone can never put a volume on the
    /// figure; keeping the rows exact is what keeps the price it reads the
    /// price of the volume in hand.
    latest_bytes: HashMap<String, usize>,
}

impl VolumeInventory {
    // ---- the still ----------------------------------------------------

    /// What the static render of `site` **at `at`** draws from, if that volume
    /// is resident.
    ///
    /// Handed back as refcounts rather than borrows: every caller clones out
    /// of the map before touching the dispatcher, because the dispatcher is
    /// borrowed mutably in the same statement.
    pub(crate) fn still_for(&self, site: &str, at: NaiveDateTime) -> Option<Still> {
        self.still
            .get(site)?
            .get(&at)
            .map(|entry| (Arc::clone(&entry.volume.0), Arc::clone(&entry.volume.1)))
    }

    /// Make `volume` what `site`'s static render draws from **at `at`**.
    ///
    /// Returns whatever [`MAX_RESIDENT_STILL_VOLUMES`] forced out, **owned**,
    /// for the caller to hand to the deferred-drop path. The volume just
    /// installed is never the one returned.
    #[must_use = "a forced-out volume is tens of megabytes; hand it to the deferred-drop path"]
    pub(crate) fn install_still(
        &mut self,
        site: String,
        at: NaiveDateTime,
        volume: Still,
    ) -> Vec<Still> {
        self.installs += 1;
        let installed = self.installs;
        let bytes = squallar_radar::scan_size::scan_bytes(&volume.0);
        self.still.entry(site).or_default().insert(
            at,
            StillEntry {
                volume,
                installed,
                bytes,
            },
        );
        self.enforce_still_cap(installed)
    }

    /// Drop least-recently-installed stills until the store is inside
    /// [`MAX_RESIDENT_STILL_VOLUMES`], never touching the install `spared`.
    fn enforce_still_cap(&mut self, spared: u64) -> Vec<Still> {
        let mut forced = Vec::new();
        while self.still_count() > MAX_RESIDENT_STILL_VOLUMES {
            let Some((site, at)) = self
                .still
                .iter()
                .flat_map(|(site, times)| {
                    times
                        .iter()
                        .map(move |(at, entry)| (site.clone(), *at, entry.installed))
                })
                .filter(|(_, _, installed)| *installed != spared)
                .min_by_key(|(_, _, installed)| *installed)
                .map(|(site, at, _)| (site, at))
            else {
                break;
            };
            log::debug!(
                "still store is over its {MAX_RESIDENT_STILL_VOLUMES}-volume cap; \
                 dropping {site} @ {at}"
            );
            if let Some(entry) = self.take_still(&site, at) {
                forced.push(entry);
            }
        }
        forced
    }

    /// Remove one still, tidying an emptied site's row so `still_count` and
    /// `newest_still_for` never walk a husk.
    fn take_still(&mut self, site: &str, at: NaiveDateTime) -> Option<Still> {
        let times = self.still.get_mut(site)?;
        // The price leaves with the entry: `StillEntry` is dropped whole here
        // and in `retain_still`, so no store outlives the volume it priced.
        let gone = times.remove(&at).map(|entry| entry.volume);
        if times.is_empty() {
            self.still.remove(site);
        }
        gone
    }

    /// The most recent moment `site` holds a still for, if any — what a pane
    /// that is on the site but has not yet been handed a `scan_info` for this
    /// volume is drawing.
    pub(crate) fn newest_still_for(&self, site: &str) -> Option<NaiveDateTime> {
        self.still.get(site)?.keys().copied().max()
    }

    /// Still volumes resident, across every site.
    pub(crate) fn still_count(&self) -> usize {
        self.still.values().map(HashMap::len).sum()
    }

    // ---- the base -----------------------------------------------------

    /// `site`'s merge base — the volume half of it, which is what
    /// [`squallar_radar::current::resolve`] takes.
    pub(crate) fn base_for(&self, site: &str) -> Option<Still> {
        self.base
            .get(site)
            .map(|(scan, declared, _)| (Arc::clone(scan), Arc::clone(declared)))
    }

    /// `site`'s merge base together with the time it was collected.
    pub(crate) fn base_with_time(&self, site: &str) -> Option<Base> {
        self.base
            .get(site)
            .map(|(scan, declared, at)| (Arc::clone(scan), Arc::clone(declared), *at))
    }

    /// When `site`'s merge base was collected, if it has one.
    pub(crate) fn base_collected_at(&self, site: &str) -> Option<NaiveDateTime> {
        self.base.get(site).map(|(_, _, at)| *at)
    }

    /// Whether `site`'s merge base is *exactly* the volume collected at `when`
    /// — the question a 3D target asks to learn it has been navigated to a
    /// volume the base still holds, rather than to a loop frame.
    pub(crate) fn base_is_from(&self, site: &str, when: NaiveDateTime) -> bool {
        self.base_collected_at(site) == Some(when)
    }

    /// Whether a volume collected at `when` would move `site`'s merge base
    /// forward. True when there is no base yet: the first complete volume
    /// always advances one.
    pub(crate) fn base_advances_to(&self, site: &str, when: NaiveDateTime) -> bool {
        self.base_collected_at(site).is_none_or(|held| when > held)
    }

    /// Make `volume` `site`'s merge base.
    pub(crate) fn install_base(&mut self, site: String, volume: Base) {
        self.base_bytes.insert(
            site.clone(),
            squallar_radar::scan_size::scan_bytes(&volume.0),
        );
        self.base.insert(site, volume);
    }

    /// Every site holding a merge base.
    pub(crate) fn sites_with_base(&self) -> impl Iterator<Item = &str> {
        self.base.keys().map(String::as_str)
    }

    // ---- the per-site latest cache's prices --------------------------

    /// Price the volume `App::latest_cached_scans` is about to file for
    /// `site`: one walk of its radials here, at arrival, and a field read
    /// every tick thereafter — the rule every holder in this file follows.
    pub(crate) fn price_latest(&mut self, site: &str, volume: &Scan) {
        self.latest_bytes.insert(
            site.to_owned(),
            squallar_radar::scan_size::scan_bytes(volume),
        );
    }

    /// Drop the price rows of the sites `doomed` names — the twin of the
    /// eviction that empties `App::latest_cached_scans` of them.
    pub(crate) fn forget_latest(&mut self, doomed: &impl Fn(&String) -> bool) {
        self.latest_bytes.retain(|site, _| !doomed(site));
    }

    /// Drop one site's price row — the twin of `App::handle_jump_to_live`
    /// moving that site's latest out of the cache and into the still store.
    pub(crate) fn forget_latest_site(&mut self, site: &str) {
        self.latest_bytes.remove(site);
    }

    /// What `site`'s latest was priced at, for the caller building
    /// [`resident_scan_bytes_with`](Self::resident_scan_bytes_with)'s rows.
    /// Zero for a site never priced, which is the under-count direction and
    /// is never reached by production: both writers of the cache price first.
    pub(crate) fn latest_price(&self, site: &str) -> usize {
        self.latest_bytes.get(site).copied().unwrap_or(0)
    }

    // ---- eviction and residency ---------------------------------------

    /// Keep only the stills `wanted` names, and hand the rest back **owned**
    /// for the deferred-drop path.
    ///
    /// This is the residency policy: `wanted` is asked of the panes, so an
    /// entry survives exactly as long as something is drawing it. It is the
    /// half of the budget that cannot evict a volume in use — the cap at
    /// install is the other half, and it is a backstop, not the policy.
    pub(crate) fn retain_still(
        &mut self,
        wanted: &impl Fn(&str, NaiveDateTime) -> bool,
    ) -> Vec<Still> {
        let mut dropped = Vec::new();
        self.still.retain(|site, times| {
            dropped.extend(
                times
                    .extract_if(|at, _| !wanted(site, *at))
                    .map(|(_, entry)| entry.volume),
            );
            !times.is_empty()
        });
        dropped
    }

    /// [`retain_still`](Self::retain_still)'s site-keyed twin for the merge
    /// bases, which name the doomed rather than the wanted.
    pub(crate) fn evict_base(&mut self, doomed: &impl Fn(&String) -> bool) -> Vec<Base> {
        self.base_bytes.retain(|site, _| !doomed(site));
        crate::app::evicted(&mut self.base, doomed)
    }

    /// **Host bytes both of this type's stores are holding**, by
    /// [`squallar_radar::scan_size::scan_bytes`] — a floor, since the
    /// allocator's own overhead is not reachable from a slice.
    ///
    /// **De-duplicated by allocation, because the two stores routinely hold
    /// the same one.** Both production arrival paths clone ONE `Arc<Scan>`
    /// into both: the archive drain in `App::update` calls
    /// [`install_base`](Self::install_base) and then
    /// [`install_still`](Self::install_still) off one binding, and
    /// `App::land_chunk_outcome` does the same for a closed live volume. The
    /// plain sum this replaced charged that volume twice, so on the ordinary
    /// one-pane scene this family read about double what the inventory held.
    ///
    /// The denominator is *these two stores*. It is what emptying them would
    /// free **if nothing else held the same volumes**, and something else
    /// almost always does: the loop download cache, a stored loop frame's
    /// hover source and the derivation memo hold `Arc`s of the same `Scan`s.
    /// So their figures and this one sum to an upper bound on the joint
    /// footprint, never to a partition of it — and emptying the inventory
    /// drops two of at least three references per volume, which on its own
    /// frees nothing.
    ///
    /// # What it costs
    ///
    /// At most [`MAX_RESIDENT_STILL_VOLUMES`] stills plus one base per site
    /// holding one, so a couple of dozen entries: `n` field reads and at most
    /// `n(n-1)/2` pointer compares, allocating nothing, on the telemetry
    /// tick. The same shape and the same argument as `squallar_gpu`'s
    /// `publish_pending_level`, which de-duplicates its own residency figure
    /// by `Arc` pointer for exactly this reason.
    ///
    /// **Which of two equal pointers pays is decided by the entries' own
    /// keys, never by iteration order.** `HashMap` iteration order is not a
    /// property this figure may rest on: "charge the first one seen" over two
    /// walks of one map is a level that could move while the heap did not.
    ///
    /// **The two stores alone, so tests only.** The census family `still
    /// scans` is a third store wider than this: production publishes it
    /// through `App::still_scan_level`, which folds in the per-site latest
    /// cache. Shipping this spelling as well would be two answers to one
    /// question, and the smaller one has no publisher.
    #[cfg(test)]
    pub(crate) fn resident_scan_bytes(&self) -> usize {
        self.resident_scan_bytes_with(std::iter::empty::<LatestVolume<'_>>())
    }

    /// [`resident_scan_bytes`](Self::resident_scan_bytes) **with the per-site
    /// latest cache folded in**, de-duplicated against both stores the same
    /// way they are against each other.
    ///
    /// `App::latest_cached_scans` is the third long-lived holder of a whole
    /// decoded volume on this side of the app and it is not this type's to
    /// own: `App::handle_jump_to_live` moves an entry out of it and into the
    /// still store, so the store lives beside the panes. It is a genuine
    /// third store all the same — evicted with these two by
    /// `App::evict_unshown_scans`, and sharing allocations with the base,
    /// because the archive drain's auto-poll arm installs one `Arc<Scan>` as
    /// the merge base and files the same one here.
    ///
    /// Taking it as an argument rather than a field is what lets the census's
    /// `still scans` family mean what its description says — "the still-pane
    /// inventory and the per-site latest cache are holding together" — with
    /// one publisher and one de-duplication.
    pub(crate) fn resident_scan_bytes_with<'s, 'a: 's>(
        &'s self,
        latest: impl Iterator<Item = LatestVolume<'a>> + Clone,
    ) -> usize {
        debug_assert_eq!(
            self.base.len(),
            self.base_bytes.len(),
            "a merge base and its price row are out of step",
        );
        let priced = || {
            self.priced_volumes().chain(
                latest.clone().map(|(site, scan, bytes)| {
                    ((site, Store::Latest, None), Arc::as_ptr(scan), bytes)
                }),
            )
        };
        priced()
            .filter(|(key, ptr, _)| {
                !priced().any(|(other, other_ptr, _)| other_ptr == *ptr && other < *key)
            })
            .fold(0usize, |sum, (_, _, bytes)| sum.saturating_add(bytes))
    }

    /// Every priced volume in both stores, as `(key, allocation, bytes)`.
    ///
    /// `key` is a **total order over the entries**, which is what lets
    /// [`resident_scan_bytes`](Self::resident_scan_bytes) name one of two
    /// equal pointers as the payer without depending on how a `HashMap`
    /// walks. A still is `(site, Still, Some(collected-at))` and a base is
    /// `(site, Base, None)`; the [`Store`] rank is what keeps two site-keyed
    /// stores apart, and within a store the rest of the key is the map's own
    /// and so is already unique.
    ///
    /// A base's price is looked up beside its volume rather than folded from
    /// `base_bytes` directly, so a price row that outlived its `Base` cannot
    /// put a volume on this figure that the inventory has dropped. The
    /// `debug_assert_eq!` above is what catches the row itself.
    ///
    /// Yielded rather than collected because the caller walks it twice and
    /// must not allocate to do so.
    fn priced_volumes(&self) -> impl Iterator<Item = PricedVolume<'_>> {
        let stills = self.still.iter().flat_map(|(site, times)| {
            times.iter().map(move |(at, entry)| {
                (
                    (site.as_str(), Store::Still, Some(*at)),
                    Arc::as_ptr(&entry.volume.0),
                    entry.bytes,
                )
            })
        });
        let bases = self.base.iter().map(move |(site, (scan, _, _))| {
            (
                (site.as_str(), Store::Base, None),
                Arc::as_ptr(scan),
                self.base_bytes.get(site).copied().unwrap_or(0),
            )
        });
        stills.chain(bases)
    }

    /// Every volume still held here, for the derived-product cache's retain
    /// sweep. Both stores, because a `Scan` reachable from either one must
    /// keep its derivations.
    pub(crate) fn resident(&self) -> impl Iterator<Item = &Scan> {
        self.still
            .values()
            .flat_map(HashMap::values)
            .map(|entry| entry.volume.0.as_ref())
            .chain(self.base.values().map(|(scan, _, _)| scan.as_ref()))
    }

    /// Drop every merge base. Test scaffolding: production drops a base only
    /// through [`evict_base`](Self::evict_base), which is bounded by what the
    /// panes are showing.
    #[cfg(test)]
    pub(crate) fn forget_all_bases(&mut self) {
        self.base_bytes.clear();
        self.base.clear();
    }

    /// Whether any site has a still volume.
    #[cfg(test)]
    pub(crate) fn holds_no_still(&self) -> bool {
        self.still.is_empty()
    }

    /// Whether `site` has anything for a static render to draw at `at`.
    /// Production asks for the volume itself ([`still_for`](Self::still_for))
    /// and never merely whether one is there; the tests assert residency
    /// directly.
    #[cfg(test)]
    pub(crate) fn holds_still(&self, site: &str, at: NaiveDateTime) -> bool {
        self.still.get(site).is_some_and(|t| t.contains_key(&at))
    }

    /// Whether `site` has any still at all, at any moment.
    #[cfg(test)]
    pub(crate) fn holds_any_still(&self, site: &str) -> bool {
        self.still.get(site).is_some_and(|t| !t.is_empty())
    }

    /// [`holds_still`](Self::holds_still) for the merge bases.
    #[cfg(test)]
    pub(crate) fn holds_base(&self, site: &str) -> bool {
        self.base.contains_key(site)
    }
}

/// One deferred-drop payload out of a decoded volume.
///
/// `offload::drain_deferred_drops` frees **at least one payload per turn**
/// whatever its budget says, so a whole volume filed as one payload is one
/// frame paying its entire teardown. These are the pieces the eviction path
/// can hand over owned instead.
///
/// The fields are never read on purpose: they exist to be *dropped*, on the
/// drain's schedule, and `Drop` is the reader.
#[allow(dead_code)]
pub(crate) enum VolumeDropPart {
    /// One sweep of a volume this process held the last reference to — a few
    /// MiB, which is what makes the drain's minimum-one turn affordable.
    Sweep(nexrad_model::data::Sweep),
    /// A volume something else still holds. Dropping this reference is a
    /// refcount decrement wherever it runs, so it travels whole.
    Shared(Arc<Scan>),
    /// The declared-Nyquist half of the pair.
    Nyquist(Arc<DeclaredNyquist>),
}

/// Split evicted volumes at their sweep seam for the deferred-drop path,
/// **each part priced at what freeing it gives back**.
///
/// Handed to `offload::discard_each`, which files every item separately: on
/// wasm each part is its own queue entry, so one drain turn frees one sweep
/// rather than one 48.88 MiB median / 74.63 MiB maximum volume (measured;
/// see [`MAX_RESIDENT_STILL_VOLUMES`]). The `Scan` shell left behind — site
/// and coverage pattern, a few KiB — is dropped here, which is the price of
/// upstream's model owning its sweeps.
///
/// The price is `offload::Priced`, so the queue's `deferred_drop_bytes` can
/// say how much of an eviction is still waiting to be freed. An owned sweep is
/// priced at its gate bytes (`scan_size::sweep_bytes`, one walk of its
/// radials, at eviction and never per frame); a shared `Arc` and the
/// Nyquist half are priced at 0 — dropping a shared reference frees nothing
/// certain, and the queue raises a 0 to the struct's own size so the entry
/// still counts as held.
pub(crate) fn volume_drop_parts(
    volumes: impl IntoIterator<Item = Still>,
) -> impl Iterator<Item = squallar_worker::offload::Priced> {
    use squallar_worker::offload::Priced;

    volumes.into_iter().flat_map(|(scan, nyquist)| {
        let mut parts: Vec<Priced> = match Arc::try_unwrap(scan) {
            Ok(scan) => scan
                .into_sweeps()
                .into_iter()
                .map(|sweep| {
                    let bytes = squallar_radar::scan_size::sweep_bytes(&sweep) as u64;
                    Priced::new(bytes, VolumeDropPart::Sweep(sweep))
                })
                .collect(),
            Err(scan) => vec![Priced::new(0, VolumeDropPart::Shared(scan))],
        };
        parts.push(Priced::new(0, VolumeDropPart::Nyquist(nyquist)));
        parts
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two volumes that are distinguishable by pointer — `ready_scan` builds a
    /// fresh one per call, so `Arc::ptr_eq` tells them apart.
    ///
    /// **This is the control, not the production case.** Nothing built here
    /// can show what happens when one allocation reaches both stores, which
    /// is what every arrival actually does — see
    /// [`one_volume_in_both_stores_is_charged_once`]. It is kept distinct on
    /// purpose: a de-duplication that collapsed unrelated volumes would show
    /// up here and nowhere else.
    ///
    /// [`one_volume_in_both_stores_is_charged_once`]:
    ///     one_volume_in_both_stores_is_charged_once
    fn two_volumes() -> (Arc<Scan>, Arc<Scan>) {
        (
            crate::volume_fixture::ready_scan(),
            crate::volume_fixture::ready_scan(),
        )
    }

    fn at(minute: u32) -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 8, 21)
            .expect("a real date")
            .and_hms_opt(12, minute, 0)
            .expect("a real time")
    }

    /// Everything wanted; the shape most of these tests install with.
    fn keep_all(_: &str, _: NaiveDateTime) -> bool {
        true
    }

    /// **The byte total follows both stores**, over an install, a second
    /// install, an eviction and a base.
    ///
    /// The stills carry their price inside `StillEntry` and the bases carry
    /// theirs in a map beside `base`, so the two halves fail differently: a
    /// still's price cannot outlive its entry (they are dropped together),
    /// while a base's is a second mutation that a future edit could forget.
    /// This is what would catch that — a `base_bytes` row left behind by
    /// `evict_base` reads as a site still holding a volume it has dropped,
    /// which on a heap census is a phantom nobody could find.
    #[test]
    fn the_inventorys_byte_total_tracks_both_stores() {
        let mut inv = VolumeInventory::default();
        assert_eq!(inv.resident_scan_bytes(), 0);

        let (first, second) = two_volumes();
        let one = squallar_radar::scan_size::scan_bytes(&first);
        assert!(
            one > 0,
            "fixture: a volume of no gates cannot price anything"
        );

        let forced = inv.install_still("KTLX".into(), at(0), (first, Arc::default()));
        assert!(forced.is_empty(), "fixture: one install cannot hit the cap");
        assert_eq!(inv.resident_scan_bytes(), one);

        let forced = inv.install_still("KTLX".into(), at(1), (second, Arc::default()));
        assert!(forced.is_empty());
        assert_eq!(inv.resident_scan_bytes(), 2 * one, "the second still");

        inv.install_base(
            "KTLX".into(),
            (crate::volume_fixture::ready_scan(), Arc::default(), at(1)),
        );
        assert_eq!(
            inv.resident_scan_bytes(),
            3 * one,
            "a merge base is a whole decoded volume and is charged as one"
        );

        // The residency policy drops what no pane wants; the price goes with
        // the entry.
        let dropped = inv.retain_still(&|_, when| when == at(0));
        assert_eq!(dropped.len(), 1);
        assert_eq!(inv.resident_scan_bytes(), 2 * one);

        inv.evict_base(&|_| true);
        assert_eq!(
            inv.resident_scan_bytes(),
            one,
            "an evicted base left its price behind"
        );

        let dropped = inv.retain_still(&|_, _| false);
        assert_eq!(dropped.len(), 1);
        assert_eq!(
            inv.resident_scan_bytes(),
            0,
            "an emptied inventory still priced"
        );
    }

    /// **One volume in both stores is one allocation and is charged once.**
    ///
    /// This is what production does, on both arrival paths: the archive drain
    /// clones one `Arc<Scan>` into `install_base` and then into
    /// `install_still`, and `App::land_chunk_outcome` does the same from one
    /// binding when a live volume closes whole. Summing the stored prices
    /// charged that one 48.9 MiB median volume twice, and nothing in this
    /// module could see it: `the_inventorys_byte_total_tracks_both_stores`
    /// builds its inputs with `two_volumes`, whose whole point is that they
    /// are DISTINCT.
    #[test]
    fn one_volume_in_both_stores_is_charged_once() {
        let mut inv = VolumeInventory::default();
        let scan = crate::volume_fixture::ready_scan();
        let one = squallar_radar::scan_size::scan_bytes(&scan);
        assert!(
            one > 0,
            "fixture: a volume of no gates cannot price anything"
        );

        let forced = inv.install_still("KTLX".into(), at(0), (Arc::clone(&scan), Arc::default()));
        assert!(forced.is_empty(), "fixture: one install cannot hit the cap");
        inv.install_base("KTLX".into(), (Arc::clone(&scan), Arc::default(), at(0)));
        assert!(
            Arc::strong_count(&scan) == 3,
            "fixture: this test is about ONE allocation in two stores, and \
             the stores are holding {} references",
            Arc::strong_count(&scan) - 1,
        );

        assert_eq!(
            inv.resident_scan_bytes(),
            one,
            "the volume both stores hold was charged twice",
        );

        // Dropping one of two references to one allocation frees nothing, so
        // the figure must not move: the bytes are still resident.
        inv.evict_base(&|_| true);
        assert_eq!(
            inv.resident_scan_bytes(),
            one,
            "the base went and the still still holds the same allocation",
        );
        let dropped = inv.retain_still(&|_, _| false);
        assert_eq!(dropped.len(), 1);
        assert_eq!(inv.resident_scan_bytes(), 0);
    }

    /// **The de-duplication is by allocation and not by store.** Two distinct
    /// stills and a base that is one of them is two volumes, not one and not
    /// three — the mixed case a dedup that collapsed a whole store, or one
    /// that collapsed nothing, would both get wrong.
    #[test]
    fn distinct_volumes_are_still_summed_beside_a_shared_one() {
        let mut inv = VolumeInventory::default();
        let (first, second) = two_volumes();
        let one = squallar_radar::scan_size::scan_bytes(&first);
        assert!(one > 0, "fixture: a volume of no gates prices nothing");
        assert_eq!(
            one,
            squallar_radar::scan_size::scan_bytes(&second),
            "fixture: the two volumes must price the same for the arithmetic \
             below to name a multiple",
        );

        drop(inv.install_still("KTLX".into(), at(0), (Arc::clone(&first), Arc::default())));
        drop(inv.install_still("KOUN".into(), at(0), (Arc::clone(&second), Arc::default())));
        assert_eq!(
            inv.resident_scan_bytes(),
            2 * one,
            "two unrelated volumes were collapsed into one",
        );

        // KTLX's base is the volume its still already holds; KOUN's is a
        // third, unrelated one.
        inv.install_base("KTLX".into(), (first, Arc::default(), at(0)));
        assert_eq!(
            inv.resident_scan_bytes(),
            2 * one,
            "a base that is the site's own still added a volume that is not \
             resident",
        );
        inv.install_base(
            "KOUN".into(),
            (crate::volume_fixture::ready_scan(), Arc::default(), at(1)),
        );
        assert_eq!(
            inv.resident_scan_bytes(),
            3 * one,
            "a base nothing else holds was not charged",
        );
    }

    /// **The per-site latest cache is a third store and is priced with the
    /// other two**, de-duplicated against them.
    ///
    /// The overlap is not hypothetical: the archive drain's auto-poll arm
    /// installs one `Arc<Scan>` as `site`'s merge base and files the SAME one
    /// in `App::latest_cached_scans`, so a figure that added the two would
    /// charge one volume twice — the defect this whole family had between its
    /// own two stores.
    #[test]
    fn the_latest_cache_is_priced_and_de_duplicated_against_the_stores() {
        let mut inv = VolumeInventory::default();
        let cached = crate::volume_fixture::ready_scan();
        let one = squallar_radar::scan_size::scan_bytes(&cached);
        assert!(one > 0, "fixture: a volume of no gates prices nothing");

        // Nothing in the inventory: the cache is the whole figure.
        let held = [("KTLX", Arc::clone(&cached), one)];
        let rows = || held.iter().map(|(site, scan, bytes)| (*site, scan, *bytes));
        assert_eq!(
            inv.resident_scan_bytes_with(rows()),
            one,
            "the per-site latest cache was priced by nothing",
        );

        // The archive drain's auto-poll arm: the same allocation is also the
        // site's merge base.
        inv.install_base("KTLX".into(), (Arc::clone(&cached), Arc::default(), at(0)));
        assert_eq!(
            inv.resident_scan_bytes_with(rows()),
            one,
            "a volume that is both the merge base and the site's latest was \
             charged twice",
        );

        // A still of a DIFFERENT volume is a second resident volume.
        let other = crate::volume_fixture::ready_scan();
        drop(inv.install_still("KTLX".into(), at(1), (other, Arc::default())));
        assert_eq!(
            inv.resident_scan_bytes_with(rows()),
            2 * one,
            "an unrelated still was collapsed into the shared volume",
        );

        // And the no-cache spelling is the same figure without it.
        assert_eq!(
            inv.resident_scan_bytes(),
            2 * one,
            "the empty-cache spelling must agree with the stores alone",
        );
    }

    /// **A latest-cache price row is written once, read by site, and pruned
    /// with the sites the cache is emptied of.** The volumes live on `App`;
    /// only their prices live here, so the two are kept in step by their
    /// callers and this is what the callers can rely on.
    #[test]
    fn latest_price_rows_follow_price_and_forget() {
        let mut inv = VolumeInventory::default();
        let scan = crate::volume_fixture::ready_scan();
        let one = squallar_radar::scan_size::scan_bytes(&scan);
        assert!(one > 0, "fixture: a volume of no gates prices nothing");

        assert_eq!(inv.latest_price("KTLX"), 0, "an unpriced site reads zero");
        inv.price_latest("KTLX", &scan);
        inv.price_latest("KOUN", &scan);
        inv.price_latest("KDMX", &scan);
        assert_eq!(inv.latest_price("KTLX"), one);
        assert_eq!(inv.latest_price("KOUN"), one);

        inv.forget_latest(&|site| site == "KTLX");
        assert_eq!(inv.latest_price("KTLX"), 0, "a forgotten site still priced");
        assert_eq!(
            inv.latest_price("KOUN"),
            one,
            "the other site was pruned too"
        );

        inv.forget_latest_site("KDMX");
        assert_eq!(inv.latest_price("KDMX"), 0, "a moved-out site still priced");

        // Re-pricing a site is a replacement, not an accumulation.
        inv.price_latest("KOUN", &scan);
        assert_eq!(inv.latest_price("KOUN"), one);
    }

    /// **The charge does not depend on which store the walk reaches first.**
    ///
    /// The figure walks two `HashMap`s and `HashMap` promises no order, so a
    /// "first one seen wins" rule could pay out of the still on one call and
    /// out of the base on the next — a level that moves while the heap does
    /// not. Here the two prices differ, so an order-dependent rule reads two
    /// different totals and only a key-ordered one reads a stable figure.
    #[test]
    fn the_shared_charge_is_stable_across_repeated_reads() {
        let mut inv = VolumeInventory::default();
        let scan = crate::volume_fixture::ready_scan();
        let one = squallar_radar::scan_size::scan_bytes(&scan);
        drop(inv.install_still("KTLX".into(), at(0), (Arc::clone(&scan), Arc::default())));
        inv.install_base("KTLX".into(), (Arc::clone(&scan), Arc::default(), at(0)));

        let readings: Vec<usize> = (0..64).map(|_| inv.resident_scan_bytes()).collect();
        assert!(
            readings.iter().all(|&r| r == one),
            "the shared volume's charge moved between reads: {readings:?}",
        );
    }

    /// The drain frees at least one payload per turn whatever its budget, so
    /// a volume that reaches the queue whole is a frame paying a whole
    /// volume's teardown. One volume must arrive as several entries.
    #[test]
    fn one_evicted_volume_files_more_than_one_queue_entry() {
        // The queue is thread-local; empty whatever an earlier test left.
        while squallar_worker::offload::drain_deferred_drops(std::time::Duration::from_secs(30)) > 0
        {
        }

        let volume: Still = (crate::volume_fixture::ready_scan(), Arc::default());
        let sweeps = volume.0.sweeps().len();
        assert!(
            sweeps > 1,
            "fixture: a one-sweep volume cannot prove a split"
        );

        // Filed exactly as wasm's `discard` files what `discard_each` hands
        // it: one queue entry per item.
        for part in volume_drop_parts(vec![volume]) {
            squallar_worker::offload::defer_drop("evicted-scan", Box::new(part));
        }
        let mut entries = 0;
        while squallar_worker::offload::drain_deferred_drops(std::time::Duration::ZERO) == 1 {
            entries += 1;
        }
        assert!(
            entries > 1,
            "one {sweeps}-sweep volume reached the drop queue as {entries} \
             entry(ies), so a single drain turn still frees a whole volume",
        );
        assert_eq!(
            entries,
            sweeps + 1,
            "every sweep and the declared-Nyquist half, each its own entry",
        );
    }

    /// The `Arc::try_unwrap` miss arm: a volume something else still holds
    /// cannot be decomposed, and must still be handed over rather than lost.
    #[test]
    fn a_volume_still_held_elsewhere_travels_whole_and_is_not_lost() {
        let scan = crate::volume_fixture::ready_scan();
        let second_holder = Arc::clone(&scan);
        let parts: Vec<squallar_worker::offload::Priced> =
            volume_drop_parts(vec![(scan, Arc::default())]).collect();
        assert_eq!(
            parts.len(),
            2,
            "a shared volume is its reference plus the nyquist half",
        );
        assert!(
            parts
                .iter()
                .filter_map(|part| part.payload.downcast_ref::<VolumeDropPart>())
                .any(|part| matches!(part, VolumeDropPart::Shared(held)
                    if Arc::ptr_eq(held, &second_holder))),
            "the shared volume's reference was dropped on the spot instead of \
             being handed over",
        );
        // A reference something else still holds frees nothing certain, so
        // it is priced at 0 and the queue's own floor is what makes it count.
        assert!(
            parts.iter().all(|part| part.bytes == 0),
            "a shared reference was priced as if dropping it freed a volume",
        );
    }

    /// **An owned sweep is priced at its gate bytes**, so the deferred-drop
    /// census says what an eviction is still holding rather than counting
    /// entries. The Nyquist half and any shared reference are priced at 0 —
    /// what dropping them frees is not this process's to claim.
    #[test]
    fn an_owned_sweep_is_priced_at_its_gate_bytes() {
        let volume: Still = (crate::volume_fixture::ready_scan(), Arc::default());
        let expected: u64 = volume
            .0
            .sweeps()
            .iter()
            .map(|sweep| squallar_radar::scan_size::sweep_bytes(sweep) as u64)
            .sum();
        assert!(expected > 0, "fixture: a volume with no gate bytes");

        let parts: Vec<squallar_worker::offload::Priced> =
            volume_drop_parts(vec![volume]).collect();
        let priced: u64 = parts.iter().map(|part| part.bytes).sum();
        assert_eq!(
            priced, expected,
            "the split's prices do not sum to the volume's own gate bytes",
        );
        assert_eq!(
            parts.iter().filter(|part| part.bytes == 0).count(),
            1,
            "exactly one part — the declared-Nyquist half — prices at zero",
        );
    }

    /// **The item.** Two panes on one site, parked at two moments, and each
    /// reads its own volume — the thing a site-keyed store physically could
    /// not do, and the reason `UNLINK_NOTE`'s "parked in the archive it holds
    /// its moment" was false.
    #[test]
    fn one_site_holds_two_moments_and_each_reader_gets_its_own() {
        let (earlier, later) = two_volumes();
        let mut inv = VolumeInventory::default();

        assert!(
            inv.install_still(
                "KTLX".to_owned(),
                at(10),
                (Arc::clone(&earlier), Arc::default())
            )
            .is_empty()
        );
        assert!(
            inv.install_still(
                "KTLX".to_owned(),
                at(15),
                (Arc::clone(&later), Arc::default())
            )
            .is_empty()
        );

        assert_eq!(
            inv.still_count(),
            2,
            "the second install for KTLX replaced the first, so two panes on \
             one site still share one volume and cannot show two moments",
        );
        assert!(
            Arc::ptr_eq(
                &inv.still_for("KTLX", at(10)).expect("the 12:10 volume").0,
                &earlier
            ),
            "the pane parked at 12:10 read the 12:15 volume",
        );
        assert!(
            Arc::ptr_eq(
                &inv.still_for("KTLX", at(15)).expect("the 12:15 volume").0,
                &later
            ),
            "the pane at 12:15 read the 12:10 volume",
        );
        assert!(
            inv.still_for("KTLX", at(20)).is_none(),
            "a moment nothing was installed for answered with some other volume",
        );
        assert_eq!(inv.resident().count(), 2, "the retain sweep sees both");
    }

    /// The residency policy: an entry survives exactly as long as a pane names
    /// it. The sibling at the *same site* is what a site-keyed doomed-predicate
    /// could not express.
    #[test]
    fn retain_keeps_the_moment_a_pane_names_and_drops_its_same_site_sibling() {
        let (earlier, later) = two_volumes();
        let mut inv = VolumeInventory::default();
        drop(inv.install_still("KTLX".to_owned(), at(10), (earlier, Arc::default())));
        drop(inv.install_still("KTLX".to_owned(), at(15), (later, Arc::default())));
        drop(inv.install_still("KOUN".to_owned(), at(10), (two_volumes().0, Arc::default())));

        let wanted = |site: &str, when: NaiveDateTime| site == "KTLX" && when == at(15);
        let dropped = inv.retain_still(&wanted);

        assert_eq!(
            dropped.len(),
            2,
            "eviction did not hand both volumes back owned"
        );
        assert!(inv.holds_still("KTLX", at(15)));
        assert!(
            !inv.holds_still("KTLX", at(10)),
            "a moment no pane is parked at kept a whole decoded volume resident",
        );
        assert!(
            !inv.holds_any_still("KOUN"),
            "an unshown site kept a whole decoded volume",
        );
        assert_eq!(
            inv.resident().count(),
            1,
            "the retain sweep would still see the evicted volumes, so their \
             derived products are never released either",
        );
    }

    /// The backstop. A frame that lands more arrivals than the cap allows
    /// cannot grow the store past it, and the volume just installed is never
    /// the one thrown out.
    #[test]
    fn the_cap_bounds_a_burst_and_never_evicts_the_arrival_that_tripped_it() {
        let mut inv = VolumeInventory::default();
        let mut forced_total = 0;
        for minute in 0..(MAX_RESIDENT_STILL_VOLUMES as u32 + 3) {
            let forced = inv.install_still(
                "KTLX".to_owned(),
                at(minute),
                (two_volumes().0, Arc::default()),
            );
            forced_total += forced.len();
            assert!(
                inv.holds_still("KTLX", at(minute)),
                "the cap evicted the volume that had just arrived",
            );
            assert!(
                inv.still_count() <= MAX_RESIDENT_STILL_VOLUMES,
                "the store is over its cap at {} volumes",
                inv.still_count(),
            );
        }
        assert_eq!(
            forced_total, 3,
            "the cap handed back the wrong number of volumes"
        );
        // Eleven installs into eight slots: the three oldest went, in order,
        // and nothing newer did. Spelled as a boundary rather than as "the
        // oldest is gone", which a policy that dropped an arbitrary three
        // would also satisfy.
        for gone in 0..3u32 {
            assert!(
                !inv.holds_still("KTLX", at(gone)),
                "install {gone} survived a cap that had to drop the three oldest",
            );
        }
        for kept in 3..(MAX_RESIDENT_STILL_VOLUMES as u32 + 3) {
            assert!(
                inv.holds_still("KTLX", at(kept)),
                "install {kept} was dropped although three older ones were resident",
            );
        }

        // Non-triviality: a cap below the pane count would break the feature
        // the key exists for, so it is pinned against the pane ceiling and not
        // against itself.
        const { assert!(MAX_RESIDENT_STILL_VOLUMES >= MAX_PANES_DESKTOP) };
    }

    /// **The two stores are two stores.** One owner holding both is exactly the
    /// shape whose defect is a write landing in one and the read going to the
    /// other, so the pin is that an install reaches its own store and *only*
    /// its own.
    #[test]
    fn a_still_install_does_not_reach_the_base_and_the_reverse() {
        let (still, base) = two_volumes();
        let mut inv = VolumeInventory::default();

        drop(inv.install_still(
            "KTLX".to_owned(),
            at(10),
            (Arc::clone(&still), Arc::default()),
        ));
        assert!(
            inv.holds_still("KTLX", at(10)),
            "the still install did not land"
        );
        assert!(
            !inv.holds_base("KTLX"),
            "installing a still volume also wrote the merge base, so a partial \
             volume would be handed to every whole-volume reader",
        );
        assert_eq!(inv.base_collected_at("KTLX"), None);

        inv.install_base(
            "KTLX".to_owned(),
            (Arc::clone(&base), Arc::default(), at(10)),
        );
        assert!(
            Arc::ptr_eq(&inv.still_for("KTLX", at(10)).expect("a still").0, &still),
            "installing the merge base overwrote the still volume, so the map \
             panes would jump to whatever the base last was",
        );
        assert!(
            Arc::ptr_eq(&inv.base_for("KTLX").expect("a base").0, &base),
            "the merge base is not the volume it was installed with",
        );
        assert_eq!(inv.base_collected_at("KTLX"), Some(at(10)));
    }

    /// The two time questions the archive drain and the 3D target ask.
    #[test]
    fn the_base_advances_forward_and_is_from_its_own_collection_time() {
        let mut inv = VolumeInventory::default();

        assert!(
            inv.base_advances_to("KTLX", at(10)),
            "the first complete volume for a site must advance a base it has \
             not got yet, or a site never gets one at all",
        );
        assert!(!inv.base_is_from("KTLX", at(10)));

        let (base, _) = two_volumes();
        inv.install_base("KTLX".to_owned(), (base, Arc::default(), at(10)));

        assert!(inv.base_is_from("KTLX", at(10)));
        assert!(!inv.base_is_from("KTLX", at(5)));
        assert!(inv.base_advances_to("KTLX", at(15)));
        assert!(
            !inv.base_advances_to("KTLX", at(5)),
            "an older archive volume walks the merge base backwards",
        );
        assert!(
            !inv.base_advances_to("KTLX", at(10)),
            "re-fetching the volume already held counts as advancing, so a \
             refresh would reinstall it over a live feed's newer sweeps",
        );
    }

    /// Eviction hands the volumes back **owned**, from both stores, and only
    /// for what the panes stopped naming.
    #[test]
    fn eviction_takes_the_doomed_sites_out_of_both_stores() {
        let mut inv = VolumeInventory::default();
        for site in ["KTLX", "KOUN"] {
            let (still, base) = two_volumes();
            drop(inv.install_still(site.to_owned(), at(10), (still, Arc::default())));
            inv.install_base(site.to_owned(), (base, Arc::default(), at(10)));
        }
        assert_eq!(
            inv.resident().count(),
            4,
            "precondition: two sites, two stores"
        );

        let doomed = |site: &String| site == "KOUN";
        assert_eq!(inv.retain_still(&|site: &str, _| site != "KOUN").len(), 1);
        assert_eq!(inv.evict_base(&doomed).len(), 1);

        assert!(
            inv.holds_still("KTLX", at(10)) && inv.holds_base("KTLX"),
            "the shown site was evicted"
        );
        assert!(
            !inv.holds_any_still("KOUN") && !inv.holds_base("KOUN"),
            "an unshown site kept a whole decoded volume in one of the stores",
        );
        assert_eq!(
            inv.resident().count(),
            2,
            "the retain sweep would still see the evicted site's volumes, so \
             their derived products are never released either",
        );
    }

    /// A site's newest moment, for the pane that is on the site but has not
    /// been handed this volume's `scan_info` yet.
    #[test]
    fn the_newest_moment_for_a_site_is_the_newest_and_an_emptied_site_has_none() {
        let mut inv = VolumeInventory::default();
        assert_eq!(inv.newest_still_for("KTLX"), None);

        drop(inv.install_still("KTLX".to_owned(), at(15), (two_volumes().0, Arc::default())));
        drop(inv.install_still("KTLX".to_owned(), at(10), (two_volumes().0, Arc::default())));
        assert_eq!(
            inv.newest_still_for("KTLX"),
            Some(at(15)),
            "install order, not collection time, decided which moment is newest",
        );

        drop(inv.retain_still(&|_, _| false));
        assert_eq!(inv.newest_still_for("KTLX"), None);
        assert!(
            inv.holds_no_still(),
            "an emptied site left a husk row behind"
        );
        assert_eq!(inv.retain_still(&keep_all).len(), 0);
    }
}
