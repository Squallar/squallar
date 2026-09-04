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
//! # What is counted, and what is not
//!
//! The moment payloads — `raw_values()`, the gate bytes themselves, which is
//! nearly all of it — plus the containers that hold them: each `Vec`'s
//! elements at their own size. A `Scan` is `Vec<Sweep>`, a `Sweep` is
//! `Vec<Radial>`, and a `Radial` is seven `Option<MomentData>` inline, so the
//! container arithmetic is `len * size_of::<T>()` at each level and is not
//! guesswork.
//!
//! What is **not** counted: the allocator's per-allocation overhead and any
//! spare capacity a `Vec` holds past its length. Both are real and neither is
//! reachable from a `&[u8]`, so this figure is a **floor** on what the volume
//! costs and is documented as one everywhere it is printed. It is not a
//! `size_of_val` — that would count the `Vec` headers and none of the bytes
//! they point at, which for this shape is off by three orders of magnitude.
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

/// A floor on the host bytes `scan` is holding. See the module note for what
/// is in the figure and what is not.
pub fn scan_bytes(scan: &Scan) -> usize {
    let sweeps = scan.sweeps();
    sweeps.iter().fold(size_of_val(sweeps), |sum, sweep| {
        sum.saturating_add(sweep_bytes(sweep))
    })
}

/// A floor on the host bytes one sweep is holding, its radials included.
fn sweep_bytes(sweep: &Sweep) -> usize {
    let radials = sweep.radials();
    radials.iter().fold(size_of_val(radials), |sum, radial| {
        sum.saturating_add(radial_bytes(radial))
    })
}

/// A floor on the gate bytes one radial's moments are holding.
///
/// The `Radial` struct's own size is charged by its owning sweep, with the
/// rest of the `Vec`'s elements — charging it here as well would count every
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
        .fold(0usize, |sum, m| sum.saturating_add(m.raw_values().len()));
    dual_pol.saturating_add(
        radial
            .clutter_filter_power()
            .map_or(0, |cfp| cfp.raw_values().len()),
    )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "scan_size/tests.rs"]
mod tests;
