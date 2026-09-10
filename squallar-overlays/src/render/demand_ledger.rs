//! **How often ranking a frame store by pane demand kept a granule that age
//! order would have taken.**
//!
//! Always on, in release as much as in a test build, for the reason every
//! counter on this path is: a store whose demand term never fires and a store
//! that has no demand term read *identically* from every other instrument this
//! application has. Without this, "the parked pane keeps its picture now" is an
//! argument rather than a reading, and on this campaign a ~94 MiB cut once
//! delivered exactly nothing because its precondition never held on the arm it
//! shipped to — its own counter read 0 B on all 530 ticks and nothing noticed
//! for a day.
//!
//! **Two figures per store, and the denominator is the one that decides whether
//! the row prints at all.** `considered` counts every eviction the byte budget
//! actually performed; `spared` counts the subset where the demand rank chose a
//! different victim than plain least-recently-used would have. A build that
//! evicted nothing has no population to report and prints no row. A build that
//! evicted and spared nothing prints a real `0`, which is a reading — it says
//! the mechanism ran and did not fire, and that is precisely the state a
//! silent row would hide.
//!
//! Blocks, never bytes: an eviction reordered costs and saves no bytes by
//! itself, because the ceiling that decides *how many* granules go is untouched
//! by the rank that decides *which*. Pricing these in bytes would add two
//! currencies that do not share a denominator.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

static MRMS_CONSIDERED: AtomicU64 = AtomicU64::new(0);
static MRMS_SPARED: AtomicU64 = AtomicU64::new(0);
static GMGSI_CONSIDERED: AtomicU64 = AtomicU64::new(0);
static GMGSI_SPARED: AtomicU64 = AtomicU64::new(0);

/// One eviction from the MRMS frame store. `spared` when the demand rank named
/// a different granule than age order would have.
pub fn note_mrms_eviction(spared: bool) {
    MRMS_CONSIDERED.fetch_add(1, Relaxed);
    if spared {
        MRMS_SPARED.fetch_add(1, Relaxed);
    }
}

/// One eviction from the GMGSI frame store — see [`note_mrms_eviction`].
pub fn note_gmgsi_eviction(spared: bool) {
    GMGSI_CONSIDERED.fetch_add(1, Relaxed);
    if spared {
        GMGSI_SPARED.fetch_add(1, Relaxed);
    }
}

/// `(considered, spared)` for the MRMS frame store, as running totals.
#[must_use]
pub fn mrms_totals() -> (u64, u64) {
    (MRMS_CONSIDERED.load(Relaxed), MRMS_SPARED.load(Relaxed))
}

/// `(considered, spared)` for the GMGSI frame store, as running totals.
#[must_use]
pub fn gmgsi_totals() -> (u64, u64) {
    (GMGSI_CONSIDERED.load(Relaxed), GMGSI_SPARED.load(Relaxed))
}

/// Zero every figure. A suite reading these counters shares them with every
/// other test in the binary, so a test that wants an exact reading takes this
/// first and does not run beside another that writes them.
#[doc(hidden)]
pub fn reset_for_test() {
    MRMS_CONSIDERED.store(0, Relaxed);
    MRMS_SPARED.store(0, Relaxed);
    GMGSI_CONSIDERED.store(0, Relaxed);
    GMGSI_SPARED.store(0, Relaxed);
}
