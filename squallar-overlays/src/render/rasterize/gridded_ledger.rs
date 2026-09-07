//! **What the gridded scatter costs, in the one quantity that decides it.**
//!
//! [`rasterize_gridded`](super::rasterize_gridded) paints one axis-aligned rect
//! per grid point, so its cost is not the cell count and not the picture size
//! but the product of the two: **pixels written**, which is the cell count times
//! whatever each cell's rect happens to cover. Nothing measured that. A cell
//! whose rect covers four pixels of a picture it shares with ten million other
//! cells reads exactly like a cell that covers one, in every counter the app
//! has, and the difference is a factor of four in the only work this function
//! does.
//!
//! Two readings, because a ratio needs a denominator:
//!
//! * [`Totals::written_px`] — every pixel the fill loop stored a colour into,
//!   counting a pixel once per store. This is *work*, not coverage: a pixel two
//!   cells both cover is two.
//! * [`Totals::picture_px`] — the pictures' own sizes, summed. `written_px /
//!   picture_px` is the overdraw ratio the campaign quotes, and it is a ratio
//!   only because both terms are here.
//!
//! [`Totals::cells`] is the third term the two are read against: written per
//! cell is what says whether a change moved the rect or moved the grid.
//!
//! **Two ledgers, deliberately.** The thread-local one is exact and private to
//! the caller's thread, which is what lets a test read a delta while other
//! tests raster on other threads — process-global counters and libtest's thread
//! pool cannot both be right. The process-wide one is the app's reading, summed
//! across every worker. Both are written from the same place, once per picture.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

/// One reading of the gridded rasterizer's cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Totals {
    /// Pictures that reached the cell loop — a raster refused before it (empty
    /// values, an unknown field, an empty window) is not one.
    pub pictures: u64,
    /// Grid cells that painted: past the no-data, transparent-colour and
    /// unprojectable guards.
    pub cells: u64,
    /// Pixel stores the fill loop performed, counted once per store.
    pub written_px: u64,
    /// `width * height` of those pictures, summed — [`Self::written_px`]'s
    /// denominator.
    pub picture_px: u64,
}

impl Totals {
    /// Written pixels per picture pixel: 1.0 is one store per pixel, and
    /// anything above it is overdraw. `None` with no picture behind it.
    pub fn overdraw(&self) -> Option<f64> {
        (self.picture_px > 0).then(|| self.written_px as f64 / self.picture_px as f64)
    }

    /// Written pixels per painted cell. `None` with no cell behind it.
    pub fn per_cell(&self) -> Option<f64> {
        (self.cells > 0).then(|| self.written_px as f64 / self.cells as f64)
    }

    /// This reading less an earlier one — what happened in between.
    pub fn since(&self, earlier: &Totals) -> Totals {
        Totals {
            pictures: self.pictures - earlier.pictures,
            cells: self.cells - earlier.cells,
            written_px: self.written_px - earlier.written_px,
            picture_px: self.picture_px - earlier.picture_px,
        }
    }
}

static PICTURES: AtomicU64 = AtomicU64::new(0);
static CELLS: AtomicU64 = AtomicU64::new(0);
static WRITTEN_PX: AtomicU64 = AtomicU64::new(0);
static PICTURE_PX: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static LOCAL: Cell<Totals> = const { Cell::new(Totals {
        pictures: 0,
        cells: 0,
        written_px: 0,
        picture_px: 0,
    }) };
}

/// Post one picture's reading. Called once per raster, off accumulators the
/// cell loop keeps in registers — never per cell and never per pixel.
pub(crate) fn record(cells: u64, written_px: u64, picture_px: u64) {
    PICTURES.fetch_add(1, Ordering::Relaxed);
    CELLS.fetch_add(cells, Ordering::Relaxed);
    WRITTEN_PX.fetch_add(written_px, Ordering::Relaxed);
    PICTURE_PX.fetch_add(picture_px, Ordering::Relaxed);
    LOCAL.with(|l| {
        let mut t = l.get();
        t.pictures += 1;
        t.cells += cells;
        t.written_px += written_px;
        t.picture_px += picture_px;
        l.set(t);
    });
}

/// Every gridded raster this process has drawn.
pub fn totals() -> Totals {
    Totals {
        pictures: PICTURES.load(Ordering::Relaxed),
        cells: CELLS.load(Ordering::Relaxed),
        written_px: WRITTEN_PX.load(Ordering::Relaxed),
        picture_px: PICTURE_PX.load(Ordering::Relaxed),
    }
}

/// Every gridded raster **this thread** has drawn. A delta over this is exact
/// under libtest's thread pool, where a delta over [`totals`] is not.
pub fn thread_totals() -> Totals {
    LOCAL.with(Cell::get)
}
