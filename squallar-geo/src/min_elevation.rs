//! The global 1°×1° **minimum**-elevation grid: 360 × 180 cells of `i16`
//! metres, and the bbox query the volume box's floor is derived from.
//!
//! It lives here, in the geodesy floor, and not with the elevation decoder
//! whose builder pass emits it. `squallar-radar` is what will read it, and
//! `squallar-elevation` is **planned** to stand above `squallar-radar` through
//! `squallar-device-profile` — the dependency the height-plan fit needs. Today
//! `squallar-elevation` declares `squallar-geo` and `image` and nothing else,
//! so the cycle is prospective, not present; its own `tests/charter.rs` asserts
//! that smaller set. Putting the grid there anyway would mean moving it the day
//! that dependency lands, and `squallar-geo` is already under radar and already
//! owns the projection functions this grid is queried alongside, so it belongs
//! here on its own merits as well.
//!
//! **Nodata is excluded from the minimum, and it is not a detail.** The grid
//! records a sentinel wherever the source DEM publishes no cell, which is every
//! ocean cell. A default radar box is ~920 km across ≈ 8° of longitude, so
//! every coastal WSR-88D overlaps ocean cells; a minimum that adopted
//! [`NODATA_M`] would answer −32,768 m and stretch an 18 km box to 50.8 km.
//! [`MinElevationGrid::min_over_bbox`] therefore skips sentinel cells and
//! answers `None` when a box holds no valid cell at all, which is the caller's
//! signal to keep its own default floor.
//!
//! Negative minima are real values and are kept: Death Valley and the Dead Sea
//! are honoured rather than clipped, which is why the cells are signed.

/// Cells across a row of the grid: one per whole degree of longitude.
pub const GRID_COLS: usize = 360;

/// Rows in the grid: one per whole degree of latitude, north to south.
pub const GRID_ROWS: usize = 180;

/// Cells in the whole grid.
pub const GRID_CELLS: usize = GRID_COLS * GRID_ROWS;

/// Bytes in a serialised grid: [`GRID_CELLS`] big-endian `i16`s, 129,600 B.
pub const GRID_BYTES: usize = GRID_CELLS * 2;

/// The value a cell carries where the source DEM publishes nothing.
///
/// `i16::MIN` rather than a plausible-looking depth precisely so that a reader
/// which forgets to exclude it produces an absurd answer instead of a subtly
/// wrong one.
pub const NODATA_M: i16 = i16::MIN;

/// The lowest metre value a real cell may carry, one above the sentinel, so a
/// genuine reading can never be mistaken for absence.
pub const MIN_REAL_M: i16 = NODATA_M + 1;

/// Grid row of a latitude: row 0 spans 89°N–90°N, row 179 spans 90°S–89°S.
///
/// Saturating at both ends, so the poles land in the rows that touch them
/// rather than off the end of the array.
#[inline]
pub fn row_of_lat(lat: f64) -> usize {
    if lat.is_nan() {
        return 0;
    }
    let row = (90.0 - lat).floor();
    if row < 0.0 {
        0
    } else if row >= GRID_ROWS as f64 {
        GRID_ROWS - 1
    } else {
        row as usize
    }
}

/// Grid column of a longitude: column 0 spans 180°W–179°W.
///
/// Wrapping, not clamping: longitude is periodic, and a box crossing the
/// antimeridian must read the cells on both sides of it rather than pile up at
/// one edge.
#[inline]
pub fn col_of_lon(lon: f64) -> usize {
    if lon.is_nan() {
        return 0;
    }
    // `rem_euclid` on a finite input lands in `0.0..360.0`; an infinity gives
    // NaN, which saturates to 0 on the cast. The `min` covers the cast only.
    let col = (lon + 180.0).floor().rem_euclid(GRID_COLS as f64);
    (col as usize).min(GRID_COLS - 1)
}

/// A borrowed, already-serialised grid.
#[derive(Clone, Copy, Debug)]
pub struct MinElevationGrid<'a> {
    bytes: &'a [u8],
}

impl<'a> MinElevationGrid<'a> {
    /// `None` unless `bytes` is exactly [`GRID_BYTES`] long — a truncated asset
    /// is a build defect, not a grid with missing rows.
    pub fn new(bytes: &'a [u8]) -> Option<Self> {
        (bytes.len() == GRID_BYTES).then_some(Self { bytes })
    }

    /// The raw cell, sentinel included. `None` off the grid.
    pub fn raw_cell(&self, row: usize, col: usize) -> Option<i16> {
        if row >= GRID_ROWS || col >= GRID_COLS {
            return None;
        }
        let at = (row * GRID_COLS + col) * 2;
        Some(i16::from_be_bytes([self.bytes[at], self.bytes[at + 1]]))
    }

    /// The cell's minimum in metres, or `None` where the DEM publishes nothing.
    pub fn cell_m(&self, row: usize, col: usize) -> Option<f64> {
        match self.raw_cell(row, col) {
            None | Some(NODATA_M) => None,
            Some(v) => Some(f64::from(v)),
        }
    }

    /// The minimum of the one cell containing `(lat, lon)`.
    pub fn at(&self, lat: f64, lon: f64) -> Option<f64> {
        self.cell_m(row_of_lat(lat), col_of_lon(lon))
    }

    /// The lowest elevation, in metres, over every cell the box touches —
    /// **excluding** cells the DEM does not publish.
    ///
    /// `None` when the box holds no valid cell (open ocean), when it is
    /// degenerate (`north < south`), or when any edge is not finite. A caller
    /// answers `None` with its own default floor; it must never substitute
    /// zero, which would clip below-sea-level ground.
    ///
    /// `west > east` is read as a box crossing the antimeridian and walks the
    /// short way round, which is the only reading under which a wrapped box
    /// covers the cells it actually covers.
    pub fn min_over_bbox(&self, west: f64, south: f64, east: f64, north: f64) -> Option<f64> {
        if !(west.is_finite() && south.is_finite() && east.is_finite() && north.is_finite()) {
            return None;
        }
        if north < south {
            return None;
        }
        let top = row_of_lat(north.min(90.0));
        let bottom = row_of_lat(south.max(-90.0));
        let first = col_of_lon(west);
        let last = col_of_lon(east);
        // A box wider than the world reads every column exactly once; anything
        // narrower walks forward from `first`, which wraps naturally.
        let cols = if east - west >= 360.0 {
            GRID_COLS
        } else {
            (last + GRID_COLS - first) % GRID_COLS + 1
        };

        let mut best: Option<f64> = None;
        for row in top..=bottom {
            for step in 0..cols {
                let col = (first + step) % GRID_COLS;
                if let Some(v) = self.cell_m(row, col) {
                    best = Some(best.map_or(v, |b: f64| b.min(v)));
                }
            }
        }
        best
    }
}

/// Accumulates per-cell minima while a builder walks the DEM, then serialises.
///
/// Shared with the offline builder rather than restated there: writer and
/// reader agreeing about byte order and row origin is the whole contract, and
/// two spellings of it is how that contract rots.
#[derive(Clone, Debug)]
pub struct MinElevationGridBuilder {
    cells: Vec<i16>,
}

impl Default for MinElevationGridBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl MinElevationGridBuilder {
    /// Every cell absent until something is observed into it.
    pub fn new() -> Self {
        Self {
            cells: vec![NODATA_M; GRID_CELLS],
        }
    }

    /// Record `height_m` as a candidate minimum for the cell holding
    /// `(lat, lon)`. `false` when the height is not a number.
    pub fn observe(&mut self, lat: f64, lon: f64, height_m: f64) -> bool {
        self.observe_cell(row_of_lat(lat), col_of_lon(lon), height_m)
    }

    /// The same, addressed by grid indices. `false` off the grid or on a NaN.
    ///
    /// The height **floors** to the metre rather than rounding: this grid's one
    /// job is to sit at or below the true ground, and a rounded −85.4 m would
    /// answer −85 m, which is above it.
    pub fn observe_cell(&mut self, row: usize, col: usize, height_m: f64) -> bool {
        if row >= GRID_ROWS || col >= GRID_COLS || height_m.is_nan() {
            return false;
        }
        let v = height_m
            .floor()
            .clamp(f64::from(MIN_REAL_M), f64::from(i16::MAX)) as i16;
        let at = row * GRID_COLS + col;
        if self.cells[at] == NODATA_M || v < self.cells[at] {
            self.cells[at] = v;
        }
        true
    }

    /// How many cells carry a real reading — the figure a builder logs, so an
    /// emit pass that walked nothing cannot report success.
    pub fn observed_cells(&self) -> usize {
        self.cells.iter().filter(|c| **c != NODATA_M).count()
    }

    /// The [`GRID_BYTES`]-long asset, big-endian, row 0 northernmost.
    pub fn finish(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(GRID_BYTES);
        for c in &self.cells {
            out.extend_from_slice(&c.to_be_bytes());
        }
        out
    }
}

/// The compiled-in grid, or `None` while no builder run has produced one.
///
/// **It is `None` today, on purpose and not by oversight.** Emitting it means
/// taking the minimum of all 26,450 Copernicus GLO-30 cells, which is the
/// terrain builder's `floor` pass against the real DEM; that run has not
/// happened. Until it does, [`min_over_bbox`] answers `None` for every box and
/// every caller keeps its own default floor — the same answer a mid-ocean box
/// gets, and a safe one.
pub const GRID_ASSET: Option<&[u8]> = None;

/// [`MinElevationGrid::min_over_bbox`] against the compiled-in asset.
///
/// `None` while [`GRID_ASSET`] is absent, so a caller must hold a floor of its
/// own regardless.
pub fn min_over_bbox(west: f64, south: f64, east: f64, north: f64) -> Option<f64> {
    MinElevationGrid::new(GRID_ASSET?)?.min_over_bbox(west, south, east, north)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grid holding one known value per named cell and nodata everywhere else.
    fn grid_with(cells: &[(f64, f64, f64)]) -> Vec<u8> {
        let mut b = MinElevationGridBuilder::new();
        for (lat, lon, h) in cells {
            assert!(b.observe(*lat, *lon, *h), "({lat},{lon}) is on the grid");
        }
        b.finish()
    }

    #[test]
    fn a_serialised_grid_is_exactly_one_hundred_and_twenty_nine_thousand_six_hundred_bytes() {
        assert_eq!(GRID_BYTES, 129_600);
        assert_eq!(MinElevationGridBuilder::new().finish().len(), GRID_BYTES);
        assert!(MinElevationGrid::new(&vec![0u8; GRID_BYTES - 2]).is_none());
        assert!(MinElevationGrid::new(&vec![0u8; GRID_BYTES + 2]).is_none());
        assert!(MinElevationGrid::new(&vec![0u8; GRID_BYTES]).is_some());
    }

    #[test]
    fn the_corner_cells_are_where_the_row_and_column_rules_say_they_are() {
        assert_eq!(row_of_lat(89.5), 0);
        assert_eq!(row_of_lat(90.0), 0);
        assert_eq!(row_of_lat(0.5), 89);
        assert_eq!(row_of_lat(-0.5), 90);
        assert_eq!(row_of_lat(-89.5), 179);
        assert_eq!(row_of_lat(-90.0), 179);
        assert_eq!(col_of_lon(-180.0), 0);
        assert_eq!(col_of_lon(-179.5), 0);
        assert_eq!(col_of_lon(-0.5), 179);
        assert_eq!(col_of_lon(0.5), 180);
        assert_eq!(col_of_lon(179.5), 359);
        // 180 is the same meridian as -180 and reads the same cell.
        assert_eq!(col_of_lon(180.0), 0);
    }

    /// The rule the whole module exists for: a sentinel is not a minimum.
    #[test]
    fn a_nodata_cell_is_excluded_from_the_minimum_rather_than_adopted() {
        // One valid land cell at (36.5, -116.5) and ocean everywhere else.
        let raw = grid_with(&[(36.5, -116.5, -86.0)]);
        let g = MinElevationGrid::new(&raw).expect("a full-length grid");

        // A box spanning four cells, only one of which is published.
        let got = g.min_over_bbox(-117.0, 36.0, -116.0, 37.0);
        assert_eq!(got, Some(-86.0), "the sentinel must not win the minimum");

        // The falsifiability control: the sentinel really is in the cells the
        // box read, so the assertion above is not vacuous.
        assert_eq!(
            g.raw_cell(row_of_lat(36.5), col_of_lon(-117.5)),
            Some(NODATA_M)
        );
        assert_eq!(g.cell_m(row_of_lat(36.5), col_of_lon(-117.5)), None);
    }

    #[test]
    fn a_box_with_no_published_cell_answers_none_rather_than_zero() {
        let raw = grid_with(&[(36.5, -116.5, -86.0)]);
        let g = MinElevationGrid::new(&raw).expect("a full-length grid");
        // Mid-Pacific.
        assert_eq!(g.min_over_bbox(-140.0, 10.0, -132.0, 18.0), None);
        // Degenerate and non-finite boxes are refused, not guessed at.
        assert_eq!(g.min_over_bbox(-117.0, 37.0, -116.0, 36.0), None);
        assert_eq!(g.min_over_bbox(f64::NAN, 36.0, -116.0, 37.0), None);
    }

    /// The `north < south` guard, at the only shape that can tell it apart from
    /// its absence.
    ///
    /// An inverted box spanning two rows answers `None` either way — `top` is
    /// below `bottom`, so `top..=bottom` is empty and the fold sees nothing.
    /// That is `None` for the wrong reason, and it leaves the guard untested.
    /// An inversion **inside one row** separates them: both edges land in row
    /// 53, the empty-range accident does not happen, and without the guard the
    /// fold would answer the cell's real minimum for a box that names no area.
    #[test]
    fn an_inversion_inside_a_single_row_is_refused_by_the_guard_and_not_by_an_empty_range() {
        let raw = grid_with(&[(36.5, -116.5, -86.0)]);
        let g = MinElevationGrid::new(&raw).expect("a full-length grid");

        assert_eq!(g.min_over_bbox(-117.0, 36.8, -116.0, 36.2), None);

        // The two controls that make that a statement about the guard. Both
        // edges are in the same row, so the row walk is NOT empty...
        assert_eq!(row_of_lat(36.8), row_of_lat(36.2));
        // ...and the same box the right way up answers the real minimum, so
        // the cell is reachable and the `None` above is the inversion.
        assert_eq!(g.min_over_bbox(-117.0, 36.2, -116.0, 36.8), Some(-86.0));
    }

    #[test]
    fn a_box_crossing_the_antimeridian_reads_the_cells_on_both_sides_of_it() {
        let raw = grid_with(&[(0.5, 179.5, 120.0), (0.5, -179.5, 40.0), (0.5, 0.5, -900.0)]);
        let g = MinElevationGrid::new(&raw).expect("a full-length grid");
        assert_eq!(g.min_over_bbox(179.0, 0.0, -179.0, 1.0), Some(40.0));
        // Control: the deep cell at lon 0.5 is real and is NOT in that box, so
        // the wrap walked the short way and not the long one.
        assert_eq!(g.at(0.5, 0.5), Some(-900.0));
        assert_eq!(g.min_over_bbox(-180.0, 0.0, 180.0, 1.0), Some(-900.0));
    }

    /// A floor must never sit above the ground, so the metre carry is a floor.
    #[test]
    fn a_fractional_depth_floors_downward_and_never_rounds_up() {
        let raw = grid_with(&[(36.5, -116.5, -85.4)]);
        let g = MinElevationGrid::new(&raw).expect("a full-length grid");
        assert_eq!(g.at(36.5, -116.5), Some(-86.0));
    }

    #[test]
    fn the_builder_keeps_the_lowest_of_everything_it_is_shown() {
        let mut b = MinElevationGridBuilder::new();
        for h in [100.0, -5.0, 3000.0, -4.0] {
            assert!(b.observe(36.5, -116.5, h));
        }
        assert_eq!(b.observed_cells(), 1);
        let raw = b.finish();
        let g = MinElevationGrid::new(&raw).expect("a full-length grid");
        assert_eq!(g.at(36.5, -116.5), Some(-5.0));
        assert!(!b.observe(36.5, -116.5, f64::NAN));
    }

    /// A real reading can never be spelled as the sentinel, so "absent" and
    /// "impossibly deep" stay distinguishable.
    #[test]
    fn a_reading_below_the_representable_floor_clamps_above_the_sentinel() {
        let mut b = MinElevationGridBuilder::new();
        assert!(b.observe(36.5, -116.5, -1.0e9));
        let raw = b.finish();
        let g = MinElevationGrid::new(&raw).expect("a full-length grid");
        assert_eq!(
            g.raw_cell(row_of_lat(36.5), col_of_lon(-116.5)),
            Some(MIN_REAL_M)
        );
        assert_eq!(g.at(36.5, -116.5), Some(f64::from(MIN_REAL_M)));
    }

    /// The state this crate is in, held as a test so the day the asset lands
    /// this reddens and points at the extremes pin below.
    #[test]
    fn the_compiled_in_grid_is_absent_until_the_builder_emits_one() {
        assert!(
            GRID_ASSET.is_none(),
            "an asset landed: drop this test, and un-ignore \
             `the_pinned_extremes_read_their_known_depths` below",
        );
        assert_eq!(min_over_bbox(-117.0, 36.0, -116.0, 37.0), None);
    }

    /// Death Valley, the Dead Sea, and a mid-ocean cell, against the real
    /// asset.
    ///
    /// The two depths are **measured, not looked up**: `gdalinfo -mm` over the
    /// two Copernicus GLO-30 COGs on 2026-08-30 (2021 Public release, the
    /// release `tools/squallar-terrain`'s tile-list pin names) reports
    /// −91.451 m for `N36_00_W117_00` and −427.834 m for `N31_00_E035_00`, and
    /// the builder floors each to the metre. Note that −91.5 m is **not** the
    /// surveyed −86 m of Badwater Basin: GLO-30 is a surface model and this is
    /// the lowest pixel in a whole degree cell.
    ///
    /// `#[ignore]`d because [`GRID_ASSET`] is `None`: the grid is emitted by
    /// `tools/squallar-terrain`'s `floor` pass over all 26,450 GLO-30 cells,
    /// and that run has not happened. Once it has, commit the asset, point
    /// `GRID_ASSET` at it, and run with
    /// `cargo test -p squallar-geo -- --ignored`.
    #[test]
    #[ignore = "GRID_ASSET is None until tools/squallar-terrain's floor pass runs on the real DEM"]
    fn the_pinned_extremes_read_their_known_depths() {
        let raw = GRID_ASSET.expect("the asset must be compiled in before this runs");
        let g = MinElevationGrid::new(raw).expect("a full-length grid");

        // The cell 36N..37N / 117W..116W, holding Badwater Basin.
        let death_valley = g.at(36.25, -116.83).expect("Death Valley is published");
        assert_eq!(
            death_valley, -92.0,
            "the Death Valley cell reads {death_valley} m; gdalinfo -mm over \
             Copernicus_DSM_COG_10_N36_00_W117_00_DEM measured −91.451 m on \
             2026-08-30, which floors to −92",
        );

        // The cell 31N..32N / 35E..36E, holding the Dead Sea shore.
        let dead_sea = g.at(31.5, 35.5).expect("the Dead Sea is published");
        assert_eq!(
            dead_sea, -428.0,
            "the Dead Sea cell reads {dead_sea} m; gdalinfo -mm over \
             Copernicus_DSM_COG_10_N31_00_E035_00_DEM measured −427.834 m on \
             2026-08-30, which floors to −428",
        );

        // Mid-Pacific: no DEM cell at all, and that must read as absence and
        // not as a sea-level zero.
        assert_eq!(g.at(15.0, -140.0), None);
        assert_eq!(
            g.raw_cell(row_of_lat(15.0), col_of_lon(-140.0)),
            Some(NODATA_M),
        );
        // A coastal box that overlaps both is answered by the land cell, which
        // is the rule the whole module exists for.
        assert_eq!(g.min_over_bbox(-118.0, 35.5, -116.0, 37.5), Some(-92.0));
    }
}
