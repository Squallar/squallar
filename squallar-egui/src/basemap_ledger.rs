//! What the self-hosted archives have actually **decoded** — the reading that
//! tells a dead basemap from a quiet one.
//!
//! **Product telemetry, not a campaign instrument**, on the terms of
//! [`crate::overlay_cache::ledger`] and [`crate::floor_ledger`]: always on, no
//! feature gate, every write one `fetch_add` with [`Relaxed`] ordering on a
//! `static`. The sentence that reports these numbers is written by
//! `squallar-app` beside the raster lines, so no formatting happens on any
//! path that increments.
//!
//! # Why this exists
//!
//! A `usize`→`u64` offset widening in the vendored PMTiles reader made the
//! self-hosted vector basemap serve **zero** tiles in a browser for as long as
//! it shipped, and every gate stayed green — including the Tier-2 browser rig,
//! which asserted boot, canvas, the worker wire and the overlay raster path
//! and never once asked whether a basemap tile had decoded. A page whose
//! basemap is entirely absent still boots, still paints a non-blank canvas
//! (the overlays draw), still attaches its worker and still satisfies every
//! conjunct of `--expect-overlay-rasters`. This ledger is the counter that
//! reading needs, and the rig's `--expect-basemap-tiles` is what makes it a
//! gate rather than a number.
//!
//! # The denominator — three counters, one floor
//!
//! Every figure here counts **one archive tile body that was decoded into a
//! [`walkers::Tile`] without error**, classified by the archive header's
//! declared `tile_type` (`crate::tile_source::ArchiveTileKind`) and never by
//! sniffing the body. One body, one increment, at the moment the decode
//! succeeded — on native that is the IO runtime's blocking pool, on wasm32 the
//! frame pump's decode budget, and the counter is the same on both.
//!
//! * [`Totals::vector_tiles`] — bodies from a **vector** archive (`tile_type =
//!   1`, MVT). This is the self-hosted basemap and nothing else. It is the
//!   floor: zero here means the map has no ground under it.
//! * [`Totals::raster_tiles`] — bodies from a **raster** archive (`tile_type`
//!   2/3/4/5), which today is the terrain hillshade. An install with terrain
//!   off legitimately reads zero, so this is reported and never gated on.
//! * [`Totals::sniffed_tiles`] — bodies from an archive whose header declared
//!   `tile_type = 0`, which are sniffed exactly as a plain HTTP tile is. **No
//!   archive this app opens says this**, so a non-zero reading is itself the
//!   finding: an archive is being served by a decoder nobody chose.
//!
//! # What is NOT in these figures
//!
//! * Plain HTTP raster tile sources. Those are not archives; there is no
//!   header to classify them by and they take `walkers::Tile::new` directly.
//! * **Restyles**, which is the one exclusion that could otherwise inflate a
//!   reading. A theme flip re-tessellates every visible tile out of the parsed
//!   cache without touching the network or the bytes, and no body is decoded,
//!   so nothing here moves. These are decodes, not paints.
//! * Failed decodes, and the archive's own "there is no tile at this address"
//!   answer. A tile that did not decode is not counted anywhere; the drain
//!   logs it.
//!
//! # These are not addable to the two raster figures
//!
//! The workspace rule is that every figure names its denominator, and that
//! `overlay rasters` and `texture uploads` are **never added** to each other.
//! This is a third denominator and it is added to neither:
//!
//! * `overlay rasters` ([`crate::overlay_cache::ledger`]) counts the
//!   whole-picture overlay texture dispatch. The basemap is not one of the
//!   layer kinds that dispatch rasterizes, so **no tile counted here is in
//!   that figure at all.**
//! * `texture uploads` (`squallar_gpu`'s `UploadTotals`) counts every egui
//!   texture delta the renderer was shown. A [`Totals::vector_tiles`] decode
//!   uploads **no texture whatever** — it produces tessellated shapes the
//!   frame draws directly — so it is not in that figure either. A
//!   [`Totals::raster_tiles`] decode does load one egui texture, so it lies
//!   *inside* the upload denominator; it is a subset of it, never a term to
//!   add to it.
//!
//! Three questions, three floors, and a sum over any two of them describes
//! nothing.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Vector (MVT) archive bodies decoded. See the module doc's denominator.
static VECTOR_TILES: AtomicU64 = AtomicU64::new(0);
/// Raster archive bodies decoded. See the module doc's denominator.
static RASTER_TILES: AtomicU64 = AtomicU64::new(0);
/// Bodies from an archive that declared nothing. See the module doc.
static SNIFFED_TILES: AtomicU64 = AtomicU64::new(0);

/// A reading of the three counters, taken together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Vector archive bodies decoded — the self-hosted basemap's MVT tiles,
    /// and the floor under any claim that the map has ground.
    pub vector_tiles: u64,
    /// Raster archive bodies decoded — the terrain hillshade today. Zero is
    /// legitimate on an install with terrain off.
    pub raster_tiles: u64,
    /// Bodies decoded out of an archive that declared no `tile_type`. Zero on
    /// every archive this app opens; non-zero is a finding.
    pub sniffed_tiles: u64,
}

impl Totals {
    /// How far along this ledger is, as one number, so a caller can tell
    /// "nothing has happened since I last looked" in a single compare.
    ///
    /// A progress sum, **not a figure to report**: the three counters have
    /// different denominators (see the module doc) and this addition is only
    /// ever compared against itself.
    fn progress(&self) -> u64 {
        self.vector_tiles + self.raster_tiles + self.sniffed_tiles
    }
}

/// Record one decoded vector (MVT) archive body.
pub fn note_vector_tile() {
    VECTOR_TILES.fetch_add(1, Relaxed);
}

/// Record one decoded raster archive body.
pub fn note_raster_tile() {
    RASTER_TILES.fetch_add(1, Relaxed);
}

/// Record one decoded body out of an archive that declared no `tile_type`.
pub fn note_sniffed_tile() {
    SNIFFED_TILES.fetch_add(1, Relaxed);
}

/// Read all three counters. Three [`Relaxed`] loads, not an atomic snapshot,
/// on the same terms as [`crate::overlay_cache::ledger::totals`].
pub fn totals() -> Totals {
    Totals {
        vector_tiles: VECTOR_TILES.load(Relaxed),
        raster_tiles: RASTER_TILES.load(Relaxed),
        sniffed_tiles: SNIFFED_TILES.load(Relaxed),
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

/// Put every counter back to zero.
///
/// **For tests only**, and not `#[cfg(test)]` for the reason
/// [`crate::floor_ledger::reset_for_test`] is not: the statics are
/// process-global, so a fixture that asserts on their absolute values must
/// reset first and must not run beside another that also does.
#[doc(hidden)]
pub fn reset_for_test() {
    for counter in [&VECTOR_TILES, &RASTER_TILES, &SNIFFED_TILES, &REPORTED] {
        counter.store(0, Relaxed);
    }
}
