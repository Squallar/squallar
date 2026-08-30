//! The FLOOR pass: a global 1°×1° minimum-elevation grid, 129,600 bytes.
//!
//! The app's 3D volume box puts its base at the true minimum ground inside the
//! box rather than at sea level, so a valley below the radar site is not clipped
//! off the bottom of the picture. That base has to be known **without a fetch,
//! without async and without invalidating the volume grid**, which means it is
//! compiled in — and 360 × 180 signed metres is 129,600 bytes, small enough to
//! be.
//!
//! **One grid cell is one COG.** GLO-30 publishes exactly one 1°×1° tile per
//! populated cell, so the pass is: minimum of each tile, into the cell that
//! tile names. Cells GLO-30 does not publish — every ocean cell — keep
//! `squallar_geo::min_elevation::NODATA_M`, which is what lets the reader tell
//! "no ground here" from "ground at zero". A coastal radar overlaps ocean cells
//! on every scan, and a minimum that adopted the sentinel would answer
//! −32,768 m.
//!
//! **The format is not defined here.** `squallar_geo::min_elevation` owns the
//! byte order, the row origin and the sentinel, and this pass drives its
//! `MinElevationGridBuilder`. Writer and reader agreeing about those three
//! things is the whole contract, and two spellings of it is how such a contract
//! rots.
//!
//! ---------------------------------------------------------------------------
//! COST, AND WHY IT IS PAID IN FULL
//!
//! `gdalinfo -mm` computes an EXACT minimum, which means reading every pixel of
//! every COG: ~26,450 tiles at tens of megabytes each. The cheap alternative —
//! `-oo OVERVIEW_LEVEL=2`, one sixty-fourth of the bytes — is **wrong for this
//! purpose and not merely coarse**: GLO-30's overviews are averaged, so the
//! minimum of an overview sits ABOVE the true minimum, and a box floor that
//! sits above the ground clips the ground. `-approx_stats` has the same defect
//! for the same reason. A floor may be too low; it may never be too high.
//!
//! The pass is therefore resumable: each tile's answer is appended to
//! `floor-minima.txt` in the work directory as it lands, and a re-run reads
//! that file and skips what is already in it.
//! ---------------------------------------------------------------------------

use std::sync::Mutex;

use squallar_geo::min_elevation::{self, GRID_BYTES, MinElevationGrid, MinElevationGridBuilder};

use crate::config::Config;
use crate::run::{capture, cmd, need, parallel};
use crate::tiles::{Cell, TileList, parse_tile_name, tile_name};
use crate::{Res, log};

/// What the emitted asset is called in the output directory.
pub const GRID_FILENAME: &str = "squallar-min-elevation-1deg.bin";

/// The resumable per-tile record, in the work directory.
pub const MINIMA_FILENAME: &str = "floor-minima.txt";

pub fn build(cfg: &Config, list: &TileList) -> Res<()> {
    need(&["gdalinfo"])?;
    std::fs::create_dir_all(&cfg.work)?;
    std::fs::create_dir_all(&cfg.out)?;

    let record = cfg.work.join(MINIMA_FILENAME);
    let mut known = parse_minima(&std::fs::read_to_string(&record).unwrap_or_default())?;
    log!(
        "floor: {} of {} cells already recorded in {}",
        known.len(),
        list.len(),
        record.display()
    );

    let todo: Vec<Cell> = list
        .sorted()
        .into_iter()
        .filter(|c| !known.iter().any(|(cell, _)| cell == c))
        .collect();

    if !todo.is_empty() {
        // Latency-bound on scattered S3 reads, exactly like the VRT phase, so
        // this scales past the core count rather than with it.
        let jobs = cfg.jobs.max(1);
        log!("floor: reading {} COGs, {jobs} jobs", todo.len());
        let sink = Mutex::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&record)?,
        );
        let failed = parallel(&todo, jobs, |cell| {
            let name = tile_name(*cell);
            let min = min_of_cog(&cfg.tile_vsis3(&name))?;
            let mut file = sink.lock().map_err(|_| "floor record lock poisoned")?;
            std::io::Write::write_all(&mut *file, format!("{name} {min}\n").as_bytes())?;
            Ok(())
        });
        if failed > 0 {
            return Err(format!("{failed} of {} COGs failed", todo.len()).into());
        }
        known = parse_minima(&std::fs::read_to_string(&record)?)?;
    }

    if known.len() != list.len() {
        return Err(format!(
            "floor: {} cells recorded but the pinned list holds {}; refusing to write a \
             grid with holes in it",
            known.len(),
            list.len()
        )
        .into());
    }

    let mut grid = MinElevationGridBuilder::new();
    for (cell, min_m) in &known {
        // The cell's CENTRE, so a tile named by its south-west corner lands in
        // the grid cell it actually covers.
        if !grid.observe(f64::from(cell.lat) + 0.5, f64::from(cell.lon) + 0.5, *min_m) {
            return Err(format!("floor: {cell:?} is not on the grid").into());
        }
    }

    let bytes = grid.finish();
    if bytes.len() != GRID_BYTES {
        return Err(format!("floor: emitted {} bytes, not {GRID_BYTES}", bytes.len()).into());
    }
    // Read the asset back through the reader that will consume it, so a grid
    // that cannot be parsed is a build failure and not a runtime one.
    let check = MinElevationGrid::new(&bytes).ok_or("floor: the emitted grid does not parse")?;
    for (cell, min_m) in &known {
        let want = min_m
            .floor()
            .clamp(f64::from(min_elevation::MIN_REAL_M), f64::from(i16::MAX));
        let got = check
            .at(f64::from(cell.lat) + 0.5, f64::from(cell.lon) + 0.5)
            .ok_or_else(|| format!("floor: {cell:?} read back as nodata"))?;
        if got != want {
            return Err(format!("floor: {cell:?} read back {got} m, wrote {want} m").into());
        }
    }

    let out = cfg.out.join(GRID_FILENAME);
    std::fs::write(&out, &bytes)?;
    log!(
        "floor: {} ({} bytes, {} cells published, {} nodata)",
        out.display(),
        bytes.len(),
        grid.observed_cells(),
        min_elevation::GRID_CELLS - grid.observed_cells()
    );
    Ok(())
}

/// The exact minimum of one raster, in its own units.
///
/// `-mm` forces a full-resolution pass; see this module's cost note for why an
/// overview or `-approx_stats` is not acceptable here.
pub fn min_of_cog(path: &str) -> Res<f64> {
    let text = capture(cmd("gdalinfo", &["-mm", "-norat", "-noct", "-nogcp", path]))?;
    parse_computed_min(&text).map_err(|e| format!("{path}: {e}").into())
}

/// Pull `Computed Min/Max=<min>,<max>` out of `gdalinfo -mm` output.
pub fn parse_computed_min(text: &str) -> Res<f64> {
    const KEY: &str = "Computed Min/Max=";
    let at = text
        .find(KEY)
        .ok_or("gdalinfo printed no `Computed Min/Max=`; was -mm passed?")?;
    let rest = &text[at + KEY.len()..];
    let field = rest.split([',', '\n']).next().unwrap_or("").trim();
    field
        .parse::<f64>()
        .map_err(|_| format!("`{field}` is not a number").into())
}

/// Parse the resumable record: one `NAME MIN` per line.
fn parse_minima(text: &str) -> Res<Vec<(Cell, f64)>> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (name, value) = line
            .rsplit_once(' ')
            .ok_or_else(|| format!("{MINIMA_FILENAME} line {}: no space", n + 1))?;
        let cell = parse_tile_name(name)
            .ok_or_else(|| format!("{MINIMA_FILENAME} line {}: {name} is not a tile", n + 1))?;
        let min: f64 = value
            .parse()
            .map_err(|_| format!("{MINIMA_FILENAME} line {}: {value} is not a number", n + 1))?;
        if out.iter().any(|(c, _): &(Cell, f64)| *c == cell) {
            continue;
        }
        out.push((cell, min));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `gdalinfo -mm -norat -noct -nogcp` output, captured 2026-08-30 on
    /// GDAL 3.13.3 against a synthetic 4x4 Float32 ENVI raster over the Death
    /// Valley cell whose sixteen values run −86.4 to 4053.7. Verbatim, with the
    /// two absolute paths shortened.
    ///
    /// A fixture and not a live call, so the parser is under test in a plain
    /// `cargo test` row; `a_synthetic_raster_is_read_through_real_gdalinfo`
    /// in `tests/pipeline.rs` closes the loop against the binary itself and is
    /// `#[ignore]`d there because it needs GDAL on PATH.
    const GDALINFO_MM: &str = r#"Driver: ENVI/ENVI .hdr Labelled
Files: synthetic.img
       synthetic.hdr
Size is 4, 4
Coordinate System is:
GEOGCRS["WGS 84",
    DATUM["World Geodetic System 1984",
        ELLIPSOID["WGS 84",6378137,298.257223563,
            LENGTHUNIT["metre",1]]],
    PRIMEM["Greenwich",0,
        ANGLEUNIT["degree",0.0174532925199433]],
    CS[ellipsoidal,2],
        AXIS["latitude",north,
            ORDER[1],
            ANGLEUNIT["degree",0.0174532925199433]],
        AXIS["longitude",east,
            ORDER[2],
            ANGLEUNIT["degree",0.0174532925199433]],
    ID["EPSG",4326]]
Data axis to CRS axis mapping: 2,1
Origin = (-117.000000000000000,37.000000000000000)
Pixel Size = (0.250000000000000,-0.250000000000000)
Image Structure Metadata:
  INTERLEAVE=BAND
Corner Coordinates:
Upper Left  (-117.0000000,  37.0000000) (117d 0' 0.00"W, 37d 0' 0.00"N)
Lower Left  (-117.0000000,  36.0000000) (117d 0' 0.00"W, 36d 0' 0.00"N)
Upper Right (-116.0000000,  37.0000000) (116d 0' 0.00"W, 37d 0' 0.00"N)
Lower Right (-116.0000000,  36.0000000) (116d 0' 0.00"W, 36d 0' 0.00"N)
Center      (-116.5000000,  36.5000000) (116d30' 0.00"W, 36d30' 0.00"N)
Band 1 Block=4x1 Type=Float32, ColorInterp=Undefined
    Computed Min/Max=-86.400,4053.700
"#;

    #[test]
    fn the_computed_minimum_is_read_and_not_the_maximum() {
        let got = parse_computed_min(GDALINFO_MM).expect("the field is present");
        assert!((got + 86.4).abs() < 1e-9, "read {got}, not -86.4");
        // Falsifiability: the maximum really is in the same field and is NOT
        // what came back, so a parser that split on the wrong side would fail.
        assert!(GDALINFO_MM.contains("4053.700"));
    }

    #[test]
    fn output_without_the_field_is_an_error_and_not_a_zero() {
        assert!(parse_computed_min("Size is 4, 4\n").is_err());
        assert!(parse_computed_min("Computed Min/Max=nope,1\n").is_err());
    }

    /// The whole pass, minus the subprocess: cell minima in, an asset out, and
    /// the reader that will consume it reading the values back.
    #[test]
    fn a_synthetic_dem_walk_emits_a_grid_the_reader_answers_from() {
        // Three cells at their **measured** minima: `gdalinfo -mm` over the
        // three COGs on 2026-08-30, GLO-30 2021 Public release. Not the
        // surveyed depths of the landmarks inside them — Badwater is −86 m and
        // the cell reads −91.451 — and not, for Colorado, the 2396.2 m minimum
        // of the committed z10 *tile*, which covers a fortieth of the cell and
        // reads 844 m higher than the cell does.
        let record = "\
Copernicus_DSM_COG_10_N36_00_W117_00_DEM -91.451
Copernicus_DSM_COG_10_N31_00_E035_00_DEM -427.834
Copernicus_DSM_COG_10_N39_00_W106_00_DEM 1552.236
";
        let parsed = parse_minima(record).expect("three well-formed lines");
        assert_eq!(parsed.len(), 3);

        let mut grid = MinElevationGridBuilder::new();
        for (cell, min_m) in &parsed {
            assert!(grid.observe(f64::from(cell.lat) + 0.5, f64::from(cell.lon) + 0.5, *min_m));
        }
        assert_eq!(grid.observed_cells(), 3);

        let bytes = grid.finish();
        assert_eq!(bytes.len(), GRID_BYTES);
        let read = MinElevationGrid::new(&bytes).expect("the emitted grid parses");

        // Floored to the metre downward, because a floor may never sit above
        // the ground.
        assert_eq!(read.at(36.25, -116.83), Some(-92.0));
        assert_eq!(read.at(31.5, 35.5), Some(-428.0));
        assert_eq!(read.at(39.5, -105.5), Some(1552.0));

        // Ocean, and the neighbours of the published cells, read as absence.
        assert_eq!(read.at(15.0, -140.0), None);
        assert_eq!(read.at(37.5, -116.5), None);
        // A box over the Death Valley cell and three unpublished neighbours
        // answers the one real minimum, not the sentinel.
        assert_eq!(read.min_over_bbox(-117.5, 36.0, -116.0, 37.5), Some(-92.0));
    }

    /// The record is deduplicated by cell, first line winning — a resumed run
    /// can append a cell it already recorded.
    ///
    /// Named because it is a policy and not an accident: the accumulator's
    /// keep-the-lower rule (pinned in `squallar_geo::min_elevation`) never sees
    /// the second line, so the two rules do not compose.
    #[test]
    fn a_cell_recorded_twice_keeps_the_first_line_and_not_the_lower_one() {
        let parsed = parse_minima(
            "Copernicus_DSM_COG_10_N36_00_W117_00_DEM -91.451\n\
             Copernicus_DSM_COG_10_N36_00_W117_00_DEM -500.0\n",
        )
        .expect("two well-formed lines");
        assert_eq!(parsed.len(), 1);
        assert!((parsed[0].1 + 91.451).abs() < 1e-9, "kept {}", parsed[0].1);
    }

    #[test]
    fn a_malformed_record_line_is_refused_rather_than_skipped() {
        assert!(parse_minima("not-a-tile 12\n").is_err());
        assert!(parse_minima("Copernicus_DSM_COG_10_N36_00_W117_00_DEM x\n").is_err());
        assert!(parse_minima("Copernicus_DSM_COG_10_N36_00_W117_00_DEM\n").is_err());
    }
}
