//! **Which arm of a gridded layer's `prepare_job` actually runs** — the live
//! cache's arm, or the frame staging store's.
//!
//! `overlay grid states` prices a gridded layer's `live` and `staged` stores
//! and says nothing about whether either is READ. Those are different
//! questions, and a store nothing reads is the only kind a cut can take
//! whole: the argument for releasing one is that no pane reaches it, and
//! `resident_source_states` cannot say that.
//!
//! Both arms are counted, **and so are their misses**, because a miss is how
//! the two failure modes are told apart: an arm asked a thousand times that
//! answers `None` every time is a store the dispatch wants and cannot use,
//! which reads identically to a store nothing wants if only the hits are
//! counted.
//!
//! **Always emitted, all-zero included.** A row of zeros is a walk that ran
//! and found nothing; no row at all is a binary without the walk.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// The three gridded layers that keep both a live cache and a frame staging
/// store. Kept as an enum rather than a `LayerId` lookup so the counters are
/// a fixed array and the note sites cost one relaxed add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridLayer {
    Mrms,
    Gmgsi,
    Model,
}

impl GridLayer {
    const ALL: [GridLayer; 3] = [GridLayer::Mrms, GridLayer::Gmgsi, GridLayer::Model];

    fn index(self) -> usize {
        match self {
            GridLayer::Mrms => 0,
            GridLayer::Gmgsi => 1,
            GridLayer::Model => 2,
        }
    }

    fn name(self) -> &'static str {
        match self {
            GridLayer::Mrms => "mrms",
            GridLayer::Gmgsi => "gmgsi",
            GridLayer::Model => "model",
        }
    }
}

/// `[live_hit, live_miss, frame_hit, frame_miss]` per layer.
static ARMS: [[AtomicU64; 4]; 3] = [
    [
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ],
    [
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ],
    [
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ],
];

/// A `prepare_job` that took the **live cache**'s arm — `ctx.frame` was `None`.
pub fn note_live(layer: GridLayer, hit: bool) {
    ARMS[layer.index()][if hit { 0 } else { 1 }].fetch_add(1, Relaxed);
}

/// A `prepare_job` that took the **frame staging store**'s arm — `ctx.frame`
/// named an instant.
pub fn note_frame(layer: GridLayer, hit: bool) {
    ARMS[layer.index()][if hit { 2 } else { 3 }].fetch_add(1, Relaxed);
}

/// One layer's four figures, in the order `note_*` writes them.
pub fn totals(layer: GridLayer) -> (u64, u64, u64, u64) {
    let a = &ARMS[layer.index()];
    (
        a[0].load(Relaxed),
        a[1].load(Relaxed),
        a[2].load(Relaxed),
        a[3].load(Relaxed),
    )
}

/// The `overlay grid arms:` line — one clause per layer, `;`-separated.
///
/// **Counts, never bytes**, and never added to `overlay grid states`: this says
/// how often a store was reached, that says what it costs.
pub fn arms_line(instance: &str) -> String {
    use core::fmt::Write;

    let mut out = String::new();
    let _ = write!(out, "overlay grid arms ({instance}):");
    for (i, layer) in GridLayer::ALL.iter().enumerate() {
        let (lh, lm, fh, fm) = totals(*layer);
        let _ = write!(
            out,
            "{} {} live {lh} hit {lm} miss, frame {fh} hit {fm} miss",
            if i == 0 { "" } else { ";" },
            layer.name(),
        );
    }
    out
}
