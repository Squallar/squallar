//! **What a decoded volume costs in host memory**, so a cache holding volumes
//! can say what it is holding.
//!
//! # Why this exists
//!
//! Four caches in this application hold whole `Arc<Scan>`s — the loop
//! download cache, the still-pane inventory, the derivation memo, and
//! whatever a pane is drawing from — and until this function existed not one
//! of them could say how many bytes that was. They are bounded by **frame
//! count**, never by bytes: a loop of thirty frames holds thirty decoded
//! volumes whatever a volume weighs. On a 1 GiB wasm page heap that is the
//! difference between a scene that fits and one that traps, and the trap gave
//! no clue which family it was because no family had a figure.
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
/// **16, and it is a choice rather than a measurement.** Neither instrument
/// this workspace owns can measure it: `squallar-alloc`'s counting
/// `GlobalAlloc` and the per-volume harness that priced the 208-volume corpus
/// both count the sizes *requested*, and a chunk header is by construction
/// what the allocator adds on top of the request. glibc's chunk rule puts it
/// at 8 or 16 bytes depending on size class and alignment, so the honest
/// statement of what is known is a factor of two.
///
/// Within that unresolvable factor, take the conservative end, because the
/// two error directions are not symmetric. This figure prices what four
/// caches are holding, and the budget model spends against it: **under-price
/// and the process is lost — over-price and a rung of quality is.** One of
/// those is recoverable and the other is not, so an uncertainty is resolved
/// toward the recoverable failure.
///
/// **What would retire the uncertainty:** an RSS-based instrument. Every
/// instrument here counts requested sizes, so no amount of care with them can
/// see a header; only reading what the OS has actually given the process —
/// `/proc/self/statm` on Linux against a controlled decode — separates the
/// allocator's overhead from the request. Until that exists this constant is
/// a documented bound, not a reading.
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
fn gate_bytes(moment: &impl DataMoment) -> usize {
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
