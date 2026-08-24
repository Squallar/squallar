//! What the whole-picture overlay pipeline has actually spent.
//!
//! **Product telemetry, not a campaign instrument.** Always on, no feature
//! gate, no debug arm. Every write is one `fetch_add` with [`Relaxed`]
//! ordering on a `static`; nothing here allocates, formats, locks or takes a
//! clock. The sentence that reports these numbers is written elsewhere — see
//! [`Totals`] — so no formatting happens on any path that increments.
//!
//! # The denominator
//!
//! **The overlay texture dispatch, and nothing else**: the ten layer kinds
//! `App::spawn_overlay_render` rasterizes. Radar declares
//! [`RenderMode::Texture`](squallar_source::handler::RenderMode) and keeps a
//! cache in the same map, but its raster comes off its own pipeline and is
//! refused by name at the overlay dispatch, so it is in none of these figures.
//! Neither are the loop frames, which never enter a pane's overlay cache.
//! The device's *total* upload cost, radar and font atlas included, is a
//! different instrument with a different denominator:
//! `squallar_gpu::egui_renderer::texture_upload::UploadTotals`. **The two are
//! never added together.**
//!
//! # Why a zero here is readable
//!
//! A counter that reads zero because the path never ran has to be
//! distinguishable from one that reads zero because the work was elided, or
//! the reading proves nothing. Three properties do that here, and each can
//! fail on its own:
//!
//! * [`Totals::dispatched`] is the floor. Zero means nothing ever asked for an
//!   overlay raster — no enabled texture layer, or no data behind one — and
//!   every figure below is then trivially zero. It is the conjunct that stops
//!   a byte reading passing vacuously, the way `out_moved > 0` does for the
//!   worker transport in `squallar_web`'s `worker_port`.
//! * [`Totals::arrived`] `==` [`Totals::pictures`] `+` [`Totals::dropped`] is
//!   an identity over the arrival path, not a hope: every response either
//!   reaches `Context::load_texture` or is thrown away before it, and the two
//!   are counted on the two sides of that branch. A count that stops adding up
//!   is a path that grew a third exit.
//! * [`Totals::picture_bytes`] is zero with [`Totals::pictures`] positive only
//!   if pictures are arriving with no pixels in them, which is a different
//!   fault from "no pictures arrived".
//!
//! Pinned by `the_ledger_separates_a_path_that_never_ran_from_one_that_moved_nothing`
//! and `every_arrival_is_either_a_picture_or_a_drop`.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Overlay rasters asked for. See [`note_dispatched`].
static DISPATCHED: AtomicU64 = AtomicU64::new(0);
/// Rasterized responses received. See [`note_arrived`].
static ARRIVED: AtomicU64 = AtomicU64::new(0);
/// Responses thrown away before their pixels were handed over.
static DROPPED: AtomicU64 = AtomicU64::new(0);
/// Pictures handed to egui.
static PICTURES: AtomicU64 = AtomicU64::new(0);
/// Bytes of those pictures.
static PICTURE_BYTES: AtomicU64 = AtomicU64::new(0);
/// Pictures put straight on screen.
static SHOWN: AtomicU64 = AtomicU64::new(0);
/// Pictures that reached the screen after their last band landed.
static PROMOTED: AtomicU64 = AtomicU64::new(0);
/// Uploads discarded mid-flight by a newer picture for the same destination.
static SUPERSEDED: AtomicU64 = AtomicU64::new(0);

/// A reading of the counters below, taken together.
///
/// **Reported, never gated on, by this crate.** `squallar-egui` is the UI layer
/// and has no frame of its own to report on; the running-total line is written
/// by `squallar-app` once a frame, from [`totals`], on the same terms
/// `squallar_volumetric::degrade` already uses for the surface-loss count. That
/// keeps every increment here free of a `log::` call and therefore free of
/// formatting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Rasters asked for — one per [`RendersInFlight::record`], which is the
    /// mark the dispatch sets and the admission bound counts.
    ///
    /// [`RendersInFlight::record`]: super::RendersInFlight::record
    pub dispatched: u64,
    /// Rasterized responses that came back. `dispatched > 0` with this at zero
    /// is a dispatch path whose answers never arrive.
    pub arrived: u64,
    /// Of [`Self::arrived`], those thrown away before `Context::load_texture`:
    /// every pane had moved past the dispatch or stopped drawing the layer, or
    /// the render produced no picture at all. **Bytes that were rasterized and
    /// never reached the GPU.**
    pub dropped: u64,
    /// Of [`Self::arrived`], those whose pixels were handed to egui. One per
    /// response, not one per pane: a grouped dispatch is one upload shared by
    /// every pane that asked, and counting it per pane would inflate the byte
    /// figure by the pane count.
    pub pictures: u64,
    /// Bytes of [`Self::pictures`] — `width × height × 4`, the picture's own
    /// size, taken at the call that hands the pixels over.
    pub picture_bytes: u64,
    /// Pictures put straight on screen, because the pane was drawing nothing
    /// for that layer yet.
    pub shown: u64,
    /// Pictures that were held until every band had landed and then shown.
    pub promoted: u64,
    /// Pictures handed to a cache that was **already** holding one, so an
    /// upload that had started was thrown away before it could be drawn. This
    /// is the quantity the `PAN_REBUILD_THRESHOLD` sweep calls a discarded
    /// hold, and the one a world-anchored tile grid is meant to make rare.
    pub superseded: u64,
}

impl Totals {
    /// Whether the overlay dispatch has run at all. **The non-vacuity floor**:
    /// every other figure is zero when this is false, so a zero read without
    /// this one cannot be told from a path that never executed.
    pub fn ran(&self) -> bool {
        self.dispatched > 0
    }

    /// Whether every arrival is accounted for on exactly one side of the
    /// upload branch. False means the arrival path grew an exit neither
    /// [`Self::pictures`] nor [`Self::dropped`] counts.
    pub fn arrivals_balance(&self) -> bool {
        self.arrived == self.pictures + self.dropped
    }

    /// Pictures that reached the screen, by either route.
    pub fn on_screen(&self) -> u64 {
        self.shown + self.promoted
    }

    /// How far along this ledger is, as one number, so a caller can tell
    /// "nothing has happened since I last looked" in a single compare.
    fn progress(&self) -> u64 {
        self.dispatched + self.arrived + self.on_screen() + self.superseded
    }
}

/// Record that an overlay raster was asked for.
pub fn note_dispatched() {
    DISPATCHED.fetch_add(1, Relaxed);
}

/// Record that a rasterized response arrived.
pub fn note_arrived() {
    ARRIVED.fetch_add(1, Relaxed);
}

/// Record that an arrival was thrown away before its pixels were handed over.
pub fn note_dropped() {
    DROPPED.fetch_add(1, Relaxed);
}

/// Record `bytes` of picture handed to egui.
pub fn note_picture(bytes: u64) {
    PICTURES.fetch_add(1, Relaxed);
    PICTURE_BYTES.fetch_add(bytes, Relaxed);
}

/// Record a picture put straight on screen.
pub fn note_shown() {
    SHOWN.fetch_add(1, Relaxed);
}

/// Record a held picture that reached the screen.
pub fn note_promoted() {
    PROMOTED.fetch_add(1, Relaxed);
}

/// Record an upload thrown away mid-flight by a newer picture.
pub fn note_superseded() {
    SUPERSEDED.fetch_add(1, Relaxed);
}

/// Read every counter.
///
/// Eight [`Relaxed`] loads, and they are **not** an atomic snapshot: a
/// concurrent increment can land between two of them. That is deliberate and
/// harmless — every one of these is written from the frame thread, and a
/// reader that has to lock to be exactly right would be paying for a
/// consistency the numbers do not need.
pub fn totals() -> Totals {
    Totals {
        dispatched: DISPATCHED.load(Relaxed),
        arrived: ARRIVED.load(Relaxed),
        dropped: DROPPED.load(Relaxed),
        pictures: PICTURES.load(Relaxed),
        picture_bytes: PICTURE_BYTES.load(Relaxed),
        shown: SHOWN.load(Relaxed),
        promoted: PROMOTED.load(Relaxed),
        superseded: SUPERSEDED.load(Relaxed),
    }
}

/// The last [`Totals::progress`] a caller was handed by [`totals_if_moved`].
static REPORTED: AtomicU64 = AtomicU64::new(0);

/// [`totals`], but only when something has happened since the last time this
/// was asked — so a caller can write the line on a frame where the pipeline
/// moved and stay silent on one where it did not.
///
/// An idle frame costs eight relaxed loads and one compare-exchange that
/// fails.
pub fn totals_if_moved() -> Option<Totals> {
    let totals = totals();
    let progress = totals.progress();
    if REPORTED.swap(progress, Relaxed) == progress {
        return None;
    }
    Some(totals)
}

/// Put every counter back to zero.
///
/// **For tests only, and the reason this is not `#[cfg(test)]`**: the
/// counters are `static`, so a test binary shares them across every test in
/// the process and a suite that read them raw would depend on the order the
/// harness happened to run in. Tests that assert on a delta take one of these
/// and a lock; see `overlay_cache::ledger_tests`.
#[doc(hidden)]
pub fn reset_for_test() {
    for counter in [
        &DISPATCHED,
        &ARRIVED,
        &DROPPED,
        &PICTURES,
        &PICTURE_BYTES,
        &SHOWN,
        &PROMOTED,
        &SUPERSEDED,
        &REPORTED,
    ] {
        counter.store(0, Relaxed);
    }
}
