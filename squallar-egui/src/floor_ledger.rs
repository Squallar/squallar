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
//!
//! # Why a paint happened — three more, and they are not a partition
//!
//! A paint count alone cannot tell a floor that is *tracking its content*
//! from a floor that is *thrashing*, and those two want opposite fixes.
//! Three further counters say which:
//!
//! * [`Totals::key_moves`] counts **asks**: one per pane per frame whose
//!   content key differed from that pane's previous frame. This is the
//!   repaint rate the content genuinely demands — a loop tick that moves the
//!   floor's picture lands here, and so does an orbit that reframed it.
//! * [`Totals::paints_on_stable_key`] counts paints the key did **not** ask
//!   for: the force latch, the deferral arm, or a completeness latch held
//!   open.
//! * [`Totals::incomplete_paints`] counts paints that committed an
//!   incomplete resolution — a pending tile or an owed raster. Each one
//!   leaves that pane permanently dirty, so a figure tracking
//!   [`Totals::strip_paints`] is a latch stuck open rather than a key moving.
//!
//! They overlap — an incomplete paint on a frame whose key also moved is
//! counted twice over — so adding them means nothing.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Floor strips painted. See the module doc's denominator.
static STRIP_PAINTS: AtomicU64 = AtomicU64::new(0);
/// Mirror passes encoded. See the module doc's denominator.
static MIRROR_RENDERS: AtomicU64 = AtomicU64::new(0);
/// Content-key moves. See the module doc.
static KEY_MOVES: AtomicU64 = AtomicU64::new(0);
/// Paints the content key did not ask for. See the module doc.
static PAINTS_ON_STABLE_KEY: AtomicU64 = AtomicU64::new(0);
/// Paints that committed an incomplete resolution. See the module doc.
static INCOMPLETE_PAINTS: AtomicU64 = AtomicU64::new(0);

/// A reading of both counters, taken together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Floor strips painted — one per pane per frame the strip really drew.
    pub strip_paints: u64,
    /// Mirror passes encoded — one per frame the mirror was re-rendered.
    pub mirror_renders: u64,
    /// Content-key moves — the repaints the content asked for.
    pub key_moves: u64,
    /// Paints on a key that had not moved.
    pub paints_on_stable_key: u64,
    /// Paints that committed an incomplete resolution.
    pub incomplete_paints: u64,
}

impl Totals {
    /// How far along this ledger is, as one number, so a caller can tell
    /// "nothing has happened since I last looked" in a single compare.
    ///
    /// **A sum over every counter, not just the two paint ones.** A key move
    /// on the deferral arm paints nothing, and a reading that skipped the
    /// line for it would hide the one frame shape most worth seeing.
    fn progress(&self) -> u64 {
        self.strip_paints
            .wrapping_add(self.mirror_renders)
            .wrapping_add(self.key_moves)
            .wrapping_add(self.paints_on_stable_key)
            .wrapping_add(self.incomplete_paints)
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

/// Record that one pane's content key differed from its previous frame's.
pub fn note_key_move() {
    KEY_MOVES.fetch_add(1, Relaxed);
}

/// Record a paint the content key did not ask for.
pub fn note_paint_on_stable_key() {
    PAINTS_ON_STABLE_KEY.fetch_add(1, Relaxed);
}

/// Record a paint that committed an incomplete resolution.
pub fn note_incomplete_paint() {
    INCOMPLETE_PAINTS.fetch_add(1, Relaxed);
}

/// Read every counter. Five [`Relaxed`] loads, not an atomic snapshot, on the
/// same terms as [`crate::overlay_cache::ledger::totals`].
pub fn totals() -> Totals {
    Totals {
        strip_paints: STRIP_PAINTS.load(Relaxed),
        mirror_renders: MIRROR_RENDERS.load(Relaxed),
        key_moves: KEY_MOVES.load(Relaxed),
        paints_on_stable_key: PAINTS_ON_STABLE_KEY.load(Relaxed),
        incomplete_paints: INCOMPLETE_PAINTS.load(Relaxed),
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
    for counter in [
        &STRIP_PAINTS,
        &MIRROR_RENDERS,
        &KEY_MOVES,
        &PAINTS_ON_STABLE_KEY,
        &INCOMPLETE_PAINTS,
        &REPORTED,
    ] {
        counter.store(0, Relaxed);
    }
}
