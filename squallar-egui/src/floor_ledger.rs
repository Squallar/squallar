//! What the 3D floor path has actually painted — and, more to the point, what
//! it has been allowed to skip.
//!
//! **Product telemetry, not a campaign instrument**, on the terms of
//! [`crate::overlay_cache::ledger`]: always on, no feature gate, every write
//! one `fetch_add` with [`Relaxed`] ordering on a `static`. The sentence that
//! reports these numbers is written by `squallar-app` beside the raster lines,
//! so no formatting happens on any path that increments.
//!
//! # The denominators — two, and they are never added
//!
//! * [`Totals::strip_paints`] counts **floor-strip paints**: one per 3D pane
//!   whose off-screen map strip was actually drawn on a frame
//!   (`Gui::draw_floor_strip`'s paint arm). Two 3D panes repainting on one
//!   frame are two. A skipped strip — content key unchanged and the last
//!   paint complete — is zero.
//! * [`Totals::mirror_renders`] counts **mirror passes encoded**: one per
//!   frame the renderer ran `render_mirror` (the second `update_buffers`, the
//!   clip rewrite and the mid-frame submit). Per frame, not per pane — one
//!   mirror pass copies every strip.
//!
//! An orbit gesture over a fully resolved floor moves neither; that is the
//! whole point of the strip cache, and these counters are how the claim is a
//! reading instead of an argument. The floor under both figures is frames
//! painted at all, which the frame ledger already counts.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Floor strips painted. See the module doc's denominator.
static STRIP_PAINTS: AtomicU64 = AtomicU64::new(0);
/// Mirror passes encoded. See the module doc's denominator.
static MIRROR_RENDERS: AtomicU64 = AtomicU64::new(0);

/// A reading of both counters, taken together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Floor strips painted — one per pane per frame the strip really drew.
    pub strip_paints: u64,
    /// Mirror passes encoded — one per frame the mirror was re-rendered.
    pub mirror_renders: u64,
}

impl Totals {
    /// How far along this ledger is, as one number, so a caller can tell
    /// "nothing has happened since I last looked" in a single compare.
    fn progress(&self) -> u64 {
        self.strip_paints + self.mirror_renders
    }
}

/// Record that one pane's floor strip was painted.
pub fn note_strip_paint() {
    STRIP_PAINTS.fetch_add(1, Relaxed);
}

/// Record that the mirror pass was encoded.
pub fn note_mirror_render() {
    MIRROR_RENDERS.fetch_add(1, Relaxed);
}

/// Read both counters. Two [`Relaxed`] loads, not an atomic snapshot, on the
/// same terms as [`crate::overlay_cache::ledger::totals`].
pub fn totals() -> Totals {
    Totals {
        strip_paints: STRIP_PAINTS.load(Relaxed),
        mirror_renders: MIRROR_RENDERS.load(Relaxed),
    }
}

/// The last [`Totals::progress`] a caller was handed by [`totals_if_moved`].
static REPORTED: AtomicU64 = AtomicU64::new(0);

/// [`totals`], but only when something has happened since the last time this
/// was asked — the telemetry writer's read, so an idle app writes no line.
pub fn totals_if_moved() -> Option<Totals> {
    let totals = totals();
    let progress = totals.progress();
    if REPORTED.swap(progress, Relaxed) == progress {
        return None;
    }
    Some(totals)
}

/// Put both counters back to zero.
///
/// **For tests only**, and not `#[cfg(test)]` for the reason
/// [`crate::overlay_cache::ledger::reset_for_test`] is not: the statics are
/// process-global, so assertions on their absolute values belong only to a
/// fresh process (the Tier-2 rig's legs). In-crate fixtures assert on the
/// `Gui`'s own per-instance probes instead.
#[doc(hidden)]
pub fn reset_for_test() {
    for counter in [&STRIP_PAINTS, &MIRROR_RENDERS, &REPORTED] {
        counter.store(0, Relaxed);
    }
}
