//! **What a decoded volume costs in host memory**, so a cache holding volumes
//! can say what it is holding.
//!
//! # Why this exists
//!
//! Caches in this application hold whole `Arc<Scan>`s, and until this
//! function existed not one of them could say how many bytes that was. They
//! are bounded by **frame count**, never by bytes: a loop of thirty frames
//! holds thirty decoded volumes whatever a volume weighs. On a 1 GiB wasm
//! page heap that is the difference between a scene that fits and one that
//! traps, and the trap gave no clue which family it was because no family
//! had a figure.
//!
//! # How many holders there are
//!
//! **Eight fields, eight allocations, seven owners of a decoded source
//! volume** — and the three numbers answer three different questions, so a
//! count with no definition beside it is not a fact. This header said "four"
//! and `squallar_egui::heap_census::Census::radar_floor` said "five"; both
//! were counting publishers rather than holders, and both undercounted.
//!
//! **Eight fields** that can hold an `Arc<Scan>` past the frame that made it:
//!
//! 1. `VolumeInventory::still` — a pane's static render source.
//! 2. `VolumeInventory::base` — the site's merge base.
//! 3. `App::latest_cached_scans` — the per-site latest, for `JumpToLive`.
//! 4. `LoopDownloadManager::scan_cache` — the loop's downloaded volumes.
//! 5. `DeriveMemo::entries` — a process-global `static`, not an `App` field.
//! 6. `VolumeAssembler::cached` — the chunk feed's built snapshot.
//! 7. `ChunkPoller::pending_closed` — closed volumes parked for an outcome,
//!    an unbounded queue.
//! 8. `SiteFeed::last_snapshot` — the bridge copy served while the poller is
//!    away on a round.
//!
//! **Eight allocations**, one apiece: no two of the eight share.
//!
//! Checked and **not** on the list, because it is the one a reader expects to
//! find there: a stored loop frame's `Arc<HoverSource>`, and the same `Arc`
//! in the pane's own frame list. Their [`crate::hover::SweepGates`] holds the
//! moments of the one sweep its picture was drawn from — all `SweepGates::at`
//! reaches — and clones them out of the volume, so it keeps no `Arc<Scan>`
//! and a decoded volume stays freeable by whoever else holds it while the
//! picture is on the glass.
//!
//! **Seven owners of a decoded source volume**, because 5 is not one:
//! `DeriveMemo` holds *synthetic* `Scan`s built by the derivation, which are
//! their own allocations and share nothing with the volume they were derived
//! from.
//!
//! Checked and **not** on the list: `RenderCache`'s `CachedRenderOutput`
//! carries an `Arc<HoverSource>`, but every one production builds comes from
//! `HoverSource::resident`, whose `sweep` is `None` — it pins no volume, and
//! `RenderCache::entry_bytes` would price one if it ever did.
//!
//! # What is counted
//!
//! Three terms, and each is what the **allocator** is holding rather than
//! what a slice's length implies:
//!
//! * **The gate bytes** — every moment's `raw_values()`, which is nearly all
//!   of the figure.
//! * **The containers, at capacity.** A `Scan` is `Vec<Sweep>`, a `Sweep` is
//!   `Vec<Radial>`, and a `Radial` is seven `Option<MomentData>` inline. Both
//!   vectors are charged `capacity() * size_of::<T>()`, through the
//!   `sweeps_capacity` / `radials_capacity` accessors the vendored
//!   `nexrad-model` carries for this (see `vendor/nexrad-model/VENDORED.md`).
//!   Capacity, not length, because the decoder grows the radial vectors radial
//!   by radial and they end up holding **~42 % spare past their length** — the
//!   allocator holds all of it, and it was 99.6 % of the gap this function had
//!   against a counting global allocator.
//! * **The allocator's per-block overhead**, one
//!   [`ALLOCATOR_BLOCK_OVERHEAD`] per allocation the walk can see, plus
//!   [`SCAN_METADATA_BLOCKS`] for the ones it cannot.
//!
//! # What is not counted, and what that is worth
//!
//! A moment's gate buffer is charged at its length: `raw_values()` yields
//! `&[u8]` and its spare capacity is not reachable. Measured over 208 real
//! archive volumes that term is **exact to 0.10 bytes of spare per moment
//! slice** — the decoder sizes those buffers from the gate count it has
//! already read — so the residual under-count is on the order of one byte per
//! ten slices against a term that is 95.9 % of the figure. Nothing is spent
//! chasing it.
//!
//! The same 208 volumes put this function's total **below** live heap on
//! every one of them, by 1.35–2.01 % (median 1.73 %), before the capacity and
//! block terms above were added. It is not a `size_of_val` — that would count
//! the `Vec` headers and none of the bytes they point at, which for this
//! shape is off by three orders of magnitude.
//!
//! # Per instance, not per shape
//!
//! **The figure is exact for the volume in hand and is not a property of the
//! bytes it was decoded from.** Capacity is not stable the way length is: two
//! decodes of the same archive object can grow their radial vectors
//! differently, and a 336-byte spread was observed inside one shape group.
//! That is right for what this is for — a cache asking what *this* resident
//! volume is holding *now* — and it is why nothing here or in the tests pins
//! a byte count for a shape.
//!
//! # What it costs to ask
//!
//! One walk of every radial, seven `Option` discriminant reads and a slice
//! length apiece — no gate is decoded and nothing is allocated. A VCP 212
//! volume is ~16 sweeps of ~720 radials, so ~80k length reads. That is
//! cheap, but it is **not free and it is not O(1)**, so nothing calls it per
//! frame: every caller prices a volume ONCE, where the volume arrives and a
//! decode has just finished, and carries a running total thereafter.

use nexrad_model::data::{DataMoment, Radial, Scan, Sweep};

/// **Bytes the allocator spends on its own bookkeeping for each block it
/// hands out**, charged once per allocation this module can see.
///
/// **16, and it is a choice rather than a measurement — but the factor of two
/// it used to be resolved against is gone.** This paragraph said no
/// instrument here could see a chunk header, because `squallar-alloc`'s
/// counting `GlobalAlloc` and the per-volume harness both count the sizes
/// *requested*. That was wrong about what is reachable: glibc's own
/// `mallinfo2` reports `uordblks` in **chunk** bytes, header included, so the
/// header is the difference between it and a known request total and needs no
/// RSS instrument at all.
///
/// **Measured on glibc 2.44** (2026-09-09), N=20,000 allocations per size,
/// `uordblks` delta less the request total: the rule is exactly
/// `chunk = max(32, roundup16(request + 8))`, so the overhead is 8 B when
/// `request % 16 == 8` and 16 B when `request % 16 == 0`, and 12 or 20 B on
/// the odd residues. **The three sizes that dominate a decoded volume split
/// across it**: 1832 B (surveillance reflectivity) and 1192 B (Doppler) cost
/// **8 B**, and 2384 B (dual-pol) costs **16 B**. Weighted over a real
/// HEAVY6 peak's whole 512 B..4 KiB population the mean is **10.9 B**, so 16
/// over-prices that family by about half.
///
/// **The constant stays 16 anyway, and the over-pricing is the point.** Two
/// error directions are not symmetric: this figure prices what four caches
/// are holding and the budget model spends against it, so **under-price and
/// the process is lost — over-price and a rung of quality is.** The measured
/// rule is also *glibc's*, and this function runs against dlmalloc on wasm
/// and a different allocator again on macOS; a per-size arithmetic derived
/// from one host's malloc would be a precise wrong answer on the other two.
/// A measurement that is right about one platform does not license a
/// cross-platform constant to follow it.
///
/// Worth, for scale: a median archive volume holds ~32,400 blocks, so this
/// term is ~519 KB against a ~48.9 MiB volume — about 1.0 %.
pub const ALLOCATOR_BLOCK_OVERHEAD: usize = 16;

/// **Allocations a decoded volume holds that this walk cannot enumerate**,
/// charged once for a scan that holds any sweeps at all.
///
/// The walk sees one block per non-empty moment buffer, one per non-empty
/// radial vector, and one for the sweep vector. Counted against the allocator
/// over 208 real archive volumes, a volume's true block count is
/// `moment slices + sweeps + 4` with a residual of 2–5 and a median of 4 —
/// so three blocks past what the walk can name. One of them is the coverage
/// pattern's `Vec<ElevationCut>`; the rest are the decode path's own.
///
/// **This term is numerically irrelevant and is here for completeness of the
/// model, not for the bytes**: three blocks is 48 bytes against a volume of
/// tens of megabytes, one part in a million. It is charged as a constant
/// rather than walked because walking it would mean more accessors on a
/// vendored crate for less than a hundred bytes.
pub const SCAN_METADATA_BLOCKS: usize = 3;

/// The host bytes `scan` is holding. See the module note for the three terms
/// and for the one residual that is not in them.
pub fn scan_bytes(scan: &Scan) -> usize {
    let capacity = scan.sweeps_capacity();
    if capacity == 0 {
        // No allocation was made for the sweep vector, so there is no block to
        // charge and no scan-level metadata to attribute to a volume that
        // holds nothing.
        return 0;
    }
    let containers = capacity.saturating_mul(size_of::<Sweep>());
    let overhead = (1 + SCAN_METADATA_BLOCKS).saturating_mul(ALLOCATOR_BLOCK_OVERHEAD);
    scan.sweeps()
        .iter()
        .fold(containers.saturating_add(overhead), |sum, sweep| {
            sum.saturating_add(sweep_bytes(sweep))
        })
}

/// **The volume's price and each sweep's own, from one walk.**
///
/// The same three terms [`scan_bytes`] charges, with the per-sweep half kept
/// rather than folded away: a caller that has to ask what a SUBSET of a
/// volume's rungs costs cannot get there from the total, and re-walking the
/// radials to find out would pay this function's cost a second time on a
/// thread that has no room for it.
///
/// The prices are `(elevation number, bytes)` in the volume's own sweep
/// order. Two sweeps can carry the same elevation number — nothing in the
/// model forbids it — so this is a list and not a map, and a caller
/// selecting by elevation number sums every entry that matches.
pub fn scan_bytes_by_sweep(scan: &Scan) -> (usize, Vec<(u8, usize)>) {
    let capacity = scan.sweeps_capacity();
    if capacity == 0 {
        return (0, Vec::new());
    }
    let containers = capacity.saturating_mul(size_of::<Sweep>());
    let overhead = (1 + SCAN_METADATA_BLOCKS).saturating_mul(ALLOCATOR_BLOCK_OVERHEAD);
    let mut prices = Vec::with_capacity(scan.sweeps().len());
    let total = scan
        .sweeps()
        .iter()
        .fold(containers.saturating_add(overhead), |sum, sweep| {
            let bytes = sweep_bytes(sweep);
            prices.push((sweep.elevation_number(), bytes));
            sum.saturating_add(bytes)
        });
    (total, prices)
}

/// The host bytes one sweep is holding, its radials included.
///
/// Public for the eviction path: a volume this process held the last
/// reference to is split at its sweep seam before it is filed for the
/// frame-paced free (`squallar-app`'s `volume_drop_parts`), and each sweep
/// is priced here as it is filed so the deferred-drop queue can say what it
/// is holding. One walk per evicted sweep, at eviction — not per frame.
///
/// The terms compose: a scan's price is the sum of its sweeps' prices plus
/// the sweep vector and the scan's own metadata blocks, so what the drop
/// queue is told it holds and what the cache was told it released are the
/// same bytes.
pub fn sweep_bytes(sweep: &Sweep) -> usize {
    let capacity = sweep.radials_capacity();
    if capacity == 0 {
        return 0;
    }
    let containers = capacity
        .saturating_mul(size_of::<Radial>())
        .saturating_add(ALLOCATOR_BLOCK_OVERHEAD);
    sweep.radials().iter().fold(containers, |sum, radial| {
        sum.saturating_add(radial_bytes(radial))
    })
}

/// The gate bytes one radial's moments are holding, and one allocator block
/// apiece.
///
/// The `Radial` struct's own size is charged by its owning sweep, with the
/// rest of the `Vec`'s slots — charging it here as well would count every
/// radial twice.
fn radial_bytes(radial: &Radial) -> usize {
    // Every moment a radial can carry, named rather than iterated: the model
    // has no iterator over them, and a moment added to the model later will
    // read as zero here until it is added to this list. That is the honest
    // failure — an undercount that names itself — rather than a silent one.
    let moments = [
        radial.reflectivity(),
        radial.velocity(),
        radial.spectrum_width(),
        radial.differential_reflectivity(),
        radial.differential_phase(),
        radial.correlation_coefficient(),
    ];
    let dual_pol = moments
        .into_iter()
        .flatten()
        .fold(0usize, |sum, m| sum.saturating_add(gate_bytes(m)));
    dual_pol.saturating_add(radial.clutter_filter_power().map_or(0, gate_bytes))
}

/// One moment's gate buffer: its bytes, and the block holding them.
///
/// An empty buffer is charged nothing at all, block included — a `Vec` of
/// zero length never asked the allocator for anything.
///
/// Public for [`crate::hover::SweepGates`], which holds the moments of one
/// sweep rather than the volume they came out of and must price them by this
/// same convention — a sweep priced one way inside a volume and another way
/// beside it would make the census's families disagree about one allocation.
pub fn gate_bytes(moment: &impl DataMoment) -> usize {
    let len = moment.raw_values().len();
    if len == 0 {
        0
    } else {
        len.saturating_add(ALLOCATOR_BLOCK_OVERHEAD)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "scan_size/tests.rs"]
mod tests;
