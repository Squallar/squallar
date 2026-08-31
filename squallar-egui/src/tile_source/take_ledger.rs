//! What **one tile take** costs the thread that performs it.
//!
//! **Product telemetry, not a campaign instrument**, on the terms of
//! [`crate::basemap_ledger`], [`crate::floor_ledger`] and
//! [`crate::overlay_cache::ledger`]: always on, no feature gate, no lock, no
//! allocation. Each take adds two clock reads and — through
//! [`squallar_device_profile::hist::AtomicHist`] — one integer bin search and
//! two relaxed `fetch_add`s. The sentence that reports these numbers is
//! written by `squallar-app`, so no formatting happens on any path that
//! records.
//!
//! # Why a *duration* ledger, when every other ledger here counts
//!
//! [`super::PUMP_TIME_BUDGET`] is checked **between** takes and never during
//! one, so a frame's tile cost is `1 ms of budget + one full unbounded take`.
//! The budget bounds the first half. **Nothing bounds, and until this ledger
//! nothing measured, the second half** — and on wasm32 that half is an MVT
//! parse plus a lyon tessellation, or an image decode plus a texture upload,
//! on the page thread, inside the frame. `super::takes()` already counted how
//! many takes happened; how much one *costs* was a quantity the app could not
//! answer about itself, which is why the constant's own doc has to reason
//! about it in prose ("a multi-millisecond tessellation") rather than quote
//! it.
//!
//! # The denominator: one take, and five families that are never added
//!
//! Every sample here is **one completion moved off a source's tile channel and
//! handled to completion** — measured at [`super::drain_up_to`]'s call of its
//! handler, which is the exact span [`super::PUMP_TIME_BUDGET`] cannot see
//! inside. It brackets the decode, the tessellation *and* the cache put,
//! because all three are on the frame thread and all three are what the
//! governor lets run unbounded.
//!
//! Per **take**, then — never per tile requested, per tile drawn, per frame or
//! per pass. A frame performs zero or more takes per source; a tile that is
//! served from cache is not a take; a tile that is asked for and never arrives
//! is not a take.
//!
//! The five families are separate histograms because their costs are
//! different quantities with different fixes, and **an aggregate over them
//! would describe none of them**:
//!
//! * [`TakeKind::Vector`] — a body from a **vector** archive (`tile_type = 1`,
//!   MVT): parse plus lyon tessellation against the committed style. The
//!   self-hosted basemap, and the take the pan-lag question is about.
//! * [`TakeKind::Raster`] — a body from a **raster** archive (`tile_type`
//!   2/3/4/5): image decode plus, for the hillshade, its remap, plus the egui
//!   texture load.
//! * [`TakeKind::Sniffed`] — a body whose decoder was chosen by sniffing
//!   rather than declared: a plain HTTP raster source (no archive header
//!   exists to declare anything), or an archive that said `tile_type = 0`. **No
//!   archive this app opens says that**, so this family is plain-HTTP sources
//!   and nothing else, exactly as [`crate::basemap_ledger::Totals`] splits it.
//! * [`TakeKind::Restyle`] — a re-tessellation out of the parsed cache after a
//!   theme flip. No network, no bytes, no parse: the tessellation half alone,
//!   which is why it is worth reading *beside* `Vector` rather than inside it.
//! * [`TakeKind::Put`] — the native arm's whole take: an `LruCache::put`. The
//!   decode and the upload happened on the IO thread. It is here as the
//!   **control**: it is what a take costs when the expensive half is not on
//!   the frame thread, which is the shape both candidate fixes for the wasm
//!   arm are trying to reach.
//!
//! The classification is by the **archive header's declared kind and never by
//! the bytes**, which is the rule [`super::decode_archive_tile`] itself obeys
//! and the one [`crate::basemap_ledger`] argues for at length: a ledger that
//! sniffed would be answering a different question from the decoder it is
//! measuring. A take whose decode **failed** is still recorded, under the
//! family it attempted — it cost the frame the same time, and excluding it
//! would make a broken archive read as a fast one.
//!
//! # These figures are not addable to any other figure in this app
//!
//! * `overlay rasters` and `texture uploads` have their own denominators and
//!   are never added to each other; this is a third quantity and is added to
//!   neither. A `Vector` take uploads no texture at all; a `Raster` take
//!   uploads one, so it lies *inside* the upload denominator as a subset,
//!   never as a term.
//! * [`crate::basemap_ledger`] counts **decodes**, which excludes restyles and
//!   failures. This counts **takes**, which include both. The two `n`s will
//!   not match and are not meant to.
//! * A take's cost is **not** a frame segment. `frame segments … pump=` (see
//!   `squallar-app`) is the whole pump phase of one frame — every source, plus
//!   whatever else the phase does. Several takes can share one `pump` sample
//!   and a `pump` sample with no take in it is the common case. Never subtract
//!   one from the other.
//!
//! **No figure recorded here ever gates CI**; the gate over this ledger is
//! [`Totals::takes`], a count.

use squallar_device_profile::hist::{AtomicHist, Hist};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Which of the five per-take cost families a take belongs to. See the module
/// doc for what each one is and why they are never added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TakeKind {
    /// MVT parse + lyon tessellation, from a declared vector archive.
    Vector,
    /// Image decode + texture load, from a declared raster archive.
    Raster,
    /// Decoder chosen by sniffing: a plain HTTP source, or `tile_type = 0`.
    Sniffed,
    /// Re-tessellation from the parsed cache after a theme flip.
    Restyle,
    /// The native arm's take: a cache put, the heavy half already done off
    /// the frame thread.
    Put,
}

/// The families in report order. The order is the line's order, and the
/// line's order is pinned by a test.
pub const FAMILIES: [TakeKind; 5] = [
    TakeKind::Vector,
    TakeKind::Raster,
    TakeKind::Sniffed,
    TakeKind::Restyle,
    TakeKind::Put,
];

impl TakeKind {
    /// This family's slot in [`FAMILIES`] and in the histogram array.
    const fn index(self) -> usize {
        match self {
            TakeKind::Vector => 0,
            TakeKind::Raster => 1,
            TakeKind::Sniffed => 2,
            TakeKind::Restyle => 3,
            TakeKind::Put => 4,
        }
    }

    /// The word this family is called in the reported line. Lowercase and
    /// stable: `.github/browser-rig/drive.py` matches on it.
    pub const fn label(self) -> &'static str {
        match self {
            TakeKind::Vector => "vector",
            TakeKind::Raster => "raster",
            TakeKind::Sniffed => "sniffed",
            TakeKind::Restyle => "restyle",
            TakeKind::Put => "put",
        }
    }
}

/// One histogram per family. `static` rather than owned by `HttpsTiles`
/// because a take is performed inside `drain_up_to`, which is a free function
/// holding a split borrow of the source and has no recorder to reach; and
/// because the app's report wants **every** source's takes in one reading, not
/// one line per live source.
static TAKES: [AtomicHist; FAMILIES.len()] = [
    AtomicHist::new(),
    AtomicHist::new(),
    AtomicHist::new(),
    AtomicHist::new(),
    AtomicHist::new(),
];

/// Record one take of `kind` that cost `micros`. The whole hot-path API.
pub fn note_take(kind: TakeKind, micros: u32) {
    TAKES[kind.index()].record(micros);
}

/// One family's reading: its histogram, which carries its own `n`, its exact
/// mean and its conservative percentiles.
pub type FamilyHist = Hist;

/// A reading of every family, taken together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Totals {
    /// Indexed by [`TakeKind::index`]; iterate with [`FAMILIES`].
    pub families: [FamilyHist; FAMILIES.len()],
}

impl Totals {
    /// This family's histogram.
    pub fn family(&self, kind: TakeKind) -> &FamilyHist {
        &self.families[kind.index()]
    }

    /// Takes recorded across every family — the count, and the **only figure
    /// here anything gates on**. A sum over the families is meaningful for the
    /// *count* (each take is in exactly one family) and meaningless for any
    /// duration, which is why no such sum is offered.
    pub fn takes(&self) -> u64 {
        self.families.iter().map(Hist::total).sum()
    }

    /// The windowed reading between two snapshots: family by family, on
    /// [`Hist::diff`]'s terms, so the counts, the exact means and the
    /// percentiles are all of the window and not of the run. **This is what
    /// the ledger is for** — a cumulative-from-boot percentile cannot be
    /// diffed, and a gesture's takes are a few dozen samples inside a run of
    /// thousands.
    pub fn diff(&self, earlier: &Totals) -> Totals {
        let mut out = *self;
        for (slot, then) in out.families.iter_mut().zip(earlier.families.iter()) {
            *slot = slot.diff(then);
        }
        out
    }
}

/// Read every family.
pub fn totals() -> Totals {
    let mut families = [Hist::new(); FAMILIES.len()];
    for (slot, hist) in families.iter_mut().zip(TAKES.iter()) {
        *slot = hist.snapshot();
    }
    Totals { families }
}

/// The last [`Totals::takes`] a caller was handed by [`totals_if_moved`].
static REPORTED: AtomicU64 = AtomicU64::new(0);

/// [`totals`], but only when a take has happened since the last time this was
/// asked — the telemetry writer's read, so an idle app writes no line.
pub fn totals_if_moved() -> Option<Totals> {
    let totals = totals();
    let takes = totals.takes();
    if REPORTED.swap(takes, Relaxed) == takes {
        return None;
    }
    Some(totals)
}

// No `reset_for_test` here, deliberately. The count ledgers have one because
// their tests assert absolute totals; these tests assert **differences** of two
// readings (`Totals::diff`), which is both what the shipped reporting does and
// the only shape that is safe against a test binary running its cases in
// parallel over process-global statics. A reset would make the difference
// unnecessary and the tests would stop exercising the windowing.

#[cfg(test)]
mod tests;
