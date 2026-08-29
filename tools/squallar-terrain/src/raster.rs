//! The RASTER archive: PMTiles v3, hillshade by default.
//!
//! A separate archive from the contours because PMTiles v3 carries ONE
//! `tile_type` byte for the whole file. MVT and PNG cannot share a container;
//! this is a format constraint, not a packaging preference.
//!
//! ---------------------------------------------------------------------------
//! THE OVERVIEW TRAP
//!
//! `gdaladdo -r average` over a terrain-RGB image averages R, G and B
//! INDEPENDENTLY. The encoding is a base-256 positional number, so averaging
//! the digits ignores every carry between them. Measured on the N39 W106 probe
//! at a single 2x reduction: max error 3289.7 m, mean 14.6 m, 14.5% of pixels
//! wrong by more than 10 m. It looks plausible and is garbage.
//!
//! `-r nearest` is exactly correct — it copies one source triple verbatim — but
//! aliases badly on shaded relief.
//!
//! The same argument applies to hillshade, whose 3x3 slope window is just as
//! non-linear: a hillshade of a downsampled DEM is not a downsampled hillshade.
//!
//! So: resample the ELEVATION, then encode, at every zoom independently. The
//! cost is a 1/(1-1/4) = 1.33x multiplier over doing the deepest zoom alone.
//! ---------------------------------------------------------------------------

use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::config::{ATTRIBUTION, Config, Encoding, TILELIST};
use crate::grid::{Extent, frac_tile_x, frac_tile_y, tile_bbox, tile_extent};
use crate::run::{capture, cmd, need, parallel, run};
use crate::tiles::{Cell, SuperCell, TileList, supercells, tile_name};
use crate::{Res, log, mbtiles, pmtiles, trgb};

pub fn build(cfg: &Config, list: &TileList) -> Res<()> {
    need(&[
        "curl",
        "gdalwarp",
        "gdalbuildvrt",
        "gdal_translate",
        "gdaldem",
        "gdalinfo",
        "sqlite3",
        "pmtiles",
    ])?;

    let stage = cfg.tmp.join("raster-stage");
    let acc = cfg.work.join("raster-acc");
    std::fs::create_dir_all(&cfg.out)?;
    std::fs::create_dir_all(&stage)?;
    std::fs::create_dir_all(&acc)?;

    let global = cfg
        .work
        .join(format!("global_z{}_elev.tif", cfg.raster_global_maxzoom));

    // Only needed for the zooms that resample from it. Building high zooms
    // alone reads the COGs directly and does not need the global raster at all,
    // which is what makes a one-cell smoke test cheap enough to run.
    if cfg.raster_minzoom <= cfg.raster_global_maxzoom {
        build_global_elev(cfg, list, &global)?;
        assert_no_dead_land_rows(list, &global)?;
    } else {
        log!(
            "min zoom {} > {}: skipping global raster",
            cfg.raster_minzoom,
            cfg.raster_global_maxzoom
        );
    }

    // Fewer jobs than cores, because each holds ~1 GB of Float32 plus its
    // encoded copy. MEMORY is the binding constraint here, not CPU.
    //
    // **This was `jobs / 4`, on the reasoning that "each warp is already
    // multi-threaded internally". MEASURED on the build box, that is false.** A
    // single z12 super-cell -- Colorado, all land, high relief, running with
    // nothing else on a 48-vCPU box -- held 1.2-2.2% CPU for its whole 2m46s.
    // That is about ONE core, not four, and its network was bursty (2 to 555
    // MB/min): the cell is latency-bound on scattered COG reads, not compute-
    // bound. Concurrency therefore scales until memory binds, and quartering it
    // was leaving the machine idle.
    //
    // Halving rather than removing the divisor, because the constraint that IS
    // real is unmeasured: ~3 GB per cell is read off the sentence above, not off
    // a profile, and CloudWatch reports no memory without an agent. `jobs / 2`
    // is ~72 GB at 48 jobs, a 2x margin against even the 128 GB members of the
    // fleet. Do not raise it further without measuring resident memory first.
    //
    // Worth what it costs: at `jobs / 4` a global hillshade run extrapolates to
    // 15.9 h; at `jobs / 2`, 9.0 h.
    let sc_jobs = (cfg.jobs / 2).max(1);
    let elev_type_co = elevation_type_option(cfg)?;

    for z in cfg.raster_minzoom..=cfg.raster_maxzoom {
        log!(
            "z{z}: enumerating {0}x{0}-tile super-cells over land",
            cfg.supercell
        );
        let mut cells = supercells(list, z, cfg.supercell);
        if let Some(f) = &cfg.only_supercell {
            cells.retain(|c| c.name.contains(f.as_str()));
        }
        log!("z{z}: {} super-cells, {sc_jobs} jobs", cells.len());

        let target = acc.join(format!("z{z}.mbtiles"));
        let lock = Mutex::new(());
        let failed = parallel(&cells, sc_jobs, |sc| {
            one_supercell(
                cfg,
                list,
                z,
                sc,
                &stage,
                &global,
                &target,
                &lock,
                &elev_type_co,
            )
        });
        if failed > 0 {
            return Err(format!("{failed} of {} super-cells failed at z{z}", cells.len()).into());
        }
    }

    log!("merging per-zoom archives");
    let combined = cfg.work.join("raster-all.mbtiles");
    let _ = std::fs::remove_file(&combined);
    for z in cfg.raster_minzoom..=cfg.raster_maxzoom {
        let f = acc.join(format!("z{z}.mbtiles"));
        if !f.exists() {
            continue;
        }
        log!("  + z{z}.mbtiles ({} bytes)", std::fs::metadata(&f)?.len());
        mbtiles::merge(&f, &combined)?;
    }
    if !combined.exists() {
        return Err("no raster tiles were produced at any zoom".into());
    }

    let enc = cfg.encoding.as_str();
    mbtiles::set_metadata(
        &combined,
        &[
            ("name", format!("squallar-terrain-{enc}")),
            ("format", cfg.tile_format.to_lowercase()),
            ("minzoom", cfg.raster_minzoom.to_string()),
            ("maxzoom", cfg.raster_maxzoom.to_string()),
            ("attribution", ATTRIBUTION.to_string()),
            (
                "description",
                format!("Copernicus GLO-30 {enc}, {}", TILELIST.release),
            ),
        ],
    )?;

    // `pmtiles convert` is the only step here GDAL cannot do: GDAL's PMTiles
    // driver is registered `-vector-` in every released version, 3.13.3
    // included, so there is no raster PMTiles writer in GDAL at all.
    log!(
        "converting to PMTiles v3 ({} tiles)",
        mbtiles::tile_count(&combined)?
    );
    let out = cfg.raster_pmtiles();
    let _ = std::fs::remove_file(&out);
    // `--tmpdir` explicitly: go-pmtiles deduplicates through a temp file, and
    // without this it lands in /tmp -- the AL2023 ROOT EBS VOLUME -- while
    // converting a hundreds-of-GB archive. `cfg.tmp` is on the instance-store
    // stripe, which is the only volume sized for it.
    run(cmd(
        "pmtiles",
        &[
            "convert",
            "--tmpdir",
            cfg.tmp.to_string_lossy().as_ref(),
            combined.to_string_lossy().as_ref(),
            out.to_string_lossy().as_ref(),
        ],
    ))?;
    pmtiles::assert_archive(&out)?;
    log!(
        "raster: {} ({} bytes)",
        out.display(),
        std::fs::metadata(&out)?.len()
    );
    Ok(())
}

/// GDAL only grew MBTiles `ELEVATION_TYPE` in 3.13.0, and even there it is only
/// a metadata label (see [`crate::trgb`]). Pass it when it exists so the archive
/// is self-describing; skip it otherwise rather than emit a warning per cell.
fn elevation_type_option(cfg: &Config) -> Res<Vec<String>> {
    if cfg.encoding != Encoding::TerrainRgb {
        return Ok(Vec::new());
    }
    let help = capture(cmd("gdalinfo", &["--format", "MBTiles"])).unwrap_or_default();
    Ok(if help.contains("ELEVATION_TYPE") {
        vec!["-co".into(), "ELEVATION_TYPE=terrain-rgb".into()]
    } else {
        Vec::new()
    })
}

/// How many COGs go in one VRT shard, so that `len` items make at most `jobs`
/// shards and every item lands in exactly one.
///
/// Ceiling division, so the remainder rides in the existing shards rather than
/// forming a `jobs + 1`th one. Never zero: `chunks(0)` panics, and a zero here
/// would come from an empty list, which the caller refuses separately.
fn shard_size(len: usize, jobs: usize) -> usize {
    len.div_ceil(jobs.max(1)).max(1)
}

/// The latitude-sorted COG list cut into at most `jobs` shards whose cuts land
/// only BETWEEN 1-degree rows, never inside one.
///
/// **A mid-row cut destroys the row it splits.** The list sorts by
/// `(lat, lon)`, so a plain `chunks()` boundary usually leaves one row's low
/// longitudes in shard `k` and the rest in shard `k + 1` — and then both shard
/// VRTs' bounding boxes cover that whole row. The COGs declare no NODATA
/// (checked on the N62 W110 header, 2026-08-29), so wherever a shard's bbox is
/// not covered by one of its own sources the shard VRT reads a VALID elevation
/// of zero. The combining `gdalbuildvrt` paints sources in list order, later
/// over earlier, each as one whole rectangle: shard `k + 1`'s implicit zeros
/// land on top of shard `k`'s real pixels across every part of the split row
/// that shard `k + 1` does not itself hold.
///
/// The published archive carried exactly that: whole 1-degree rows of dead
/// hillshade at N41/N43/N46/N51/N53/N58/N60/N62/N64 — precisely the split rows
/// of the 48-shard build over the 26,450-COG list, minus the two (N49 at W126,
/// N56 at W161) whose destroyed western segment is open ocean. Reproduced
/// locally 2026-08-29 with 8 COGs around a mimicked N62 boundary: the sharded
/// mosaic's boundary row reads a constant zero where the flat single-VRT
/// control reads 155–464 m.
///
/// Row-aligned cuts make consecutive shards' extents DISJOINT latitude bands,
/// so no shard's rectangle can reach a row another shard owns. Pinned by
/// `no_latitude_row_is_ever_split_across_shards` and
/// `row_aligned_shards_cover_every_cog_exactly_once`.
///
/// Each shard extends FORWARD to the end of the row it would have cut, so
/// every shard except the last holds at least the ceiling-division target and
/// the shard count never exceeds `jobs`. The worst extension is one row minus
/// one COG (359) against a 552-COG target on the real list — bounded, and the
/// phase it feeds is network-latency-bound, not size-bound.
fn shard_rows(tiles: &[Cell], jobs: usize) -> Vec<&[Cell]> {
    let target = shard_size(tiles.len(), jobs);
    let mut shards = Vec::new();
    let mut start = 0;
    while start < tiles.len() {
        let mut end = (start + target).min(tiles.len());
        while end < tiles.len() && tiles[end].lat == tiles[end - 1].lat {
            end += 1;
        }
        shards.push(&tiles[start..end]);
        start = end;
    }
    shards
}

/// GLO-30's pixels per degree at full resolution, from the COG headers.
///
/// Every tile is 3600 rows to the degree. Columns thin towards the poles --
/// measured 3600 at N39 and S45, 1800 at N60, 720 at N80 -- so this is the
/// *row* figure, which is the one that does not vary.
const GLO30_PX_PER_DEGREE: u32 = 3600;

/// How many overview levels a GLO-30 COG carries.
///
/// Read from the headers, not from the product spec: 1800 / 900 / 450 rows, on
/// all four of N39 W106, N60 E010, N80 W060 and S45 W070. The count is the same
/// at every latitude even though the column counts are not, so level 2 is the 8x
/// reduction everywhere.
const GLO30_OVERVIEW_LEVELS: u32 = 3;

/// The deepest COG overview level still finer than a `target_px`-wide global
/// grid, or `None` to read full resolution.
///
/// **Deepest-that-is-still-finer, never merely deepest.** Going one level too
/// far would hand the warp a mosaic coarser than the grid it feeds, which is
/// upsampling: it would invent detail rather than skip detail the output cannot
/// hold. So the search walks outward from full resolution and stops before it
/// would cross the target.
///
/// Returns a level for GDAL's `OVERVIEW_LEVEL` open option, which is 0-based on
/// the *first* overview -- level 0 is the 2x reduction, so level `n` is
/// `2^(n+1)`.
fn global_overview_level(target_px: u32) -> Option<u32> {
    (0..GLO30_OVERVIEW_LEVELS).rfind(|level| {
        let reduction = 1u32 << (level + 1);
        (360 * GLO30_PX_PER_DEGREE) / reduction >= target_px
    })
}

/// The VRT naming every COG, built by `jobs` shards in parallel.
///
/// **This phase is pure network latency and it must be parallel.** Building it
/// as one `gdalbuildvrt` over the whole list was MEASURED on the build box at a
/// dead-flat 14,667 B/s and 0.13% of 48 vCPUs for 91 minutes: the process is
/// asleep on cross-Atlantic round trips, because the DEM bucket is
/// `eu-central-1`, the build box is not, and `gdalbuildvrt` opens its sources
/// strictly one at a time. Each open reads one 16,384-byte range, so the whole
/// list is on the order of eight hours of doing nothing. It is the only phase
/// here that gets slower the more of the planet you ask for while using none of
/// the machine. Sharding is not a tuning knob; it is the difference between
/// minutes and a third of the dead-man window.
///
/// The shards are combined by a second `gdalbuildvrt` over the part VRTs, which
/// opens `jobs` local files and no COGs at all.
///
/// **`-resolution highest` is shard-invariant, which is why it is used.** The
/// default is `average`, and the average of per-shard averages is not the global
/// average -- so sharding alone would silently move the mosaic's pixel size.
/// `highest` is a minimum, and a minimum of minima is the global minimum, so the
/// sharded VRT and a single-pass one describe the same raster.
///
/// **`-oo OVERVIEW_LEVEL` is what stops the warp reading the whole DEM, and it
/// cost a night's compute to learn.** A VRT does not expose its sources'
/// overviews, so `gdalwarp` reads the level the VRT declares. MEASURED on the
/// build box: without this, the global z8 warp pulled **240 GB in 3.75 hours at
/// a flat 21 MB/s and 1% CPU**, on course for roughly 650 GB and six more hours,
/// because it was reading near-full-resolution pixels to make a 65,536 px grid.
///
/// The arithmetic it was missing: GLO-30 is 3600 px per degree, so the full
/// mosaic is 1,296,000 px around against a 65,536 px target -- **19.8x more
/// detail than the output can hold**. Opening each COG at its 8x overview gives
/// a 162,000 px mosaic, still 2.5x finer than the target, for about 21 GB
/// instead of 650. Nothing is lost that the warp was not going to throw away.
///
/// The level is derived from the target grid by [`global_overview_level`] rather
/// than hardcoded, because it is only correct relative to
/// `raster_global_maxzoom`; a deeper global zoom needs a shallower overview.
///
/// This applies to the GLOBAL mosaic alone. The per-super-cell VRTs at high
/// zooms are built full-resolution, which is correct -- there the target grid is
/// finer than the source and there is nothing to skip.
fn build_global_vrt(cfg: &Config, list: &TileList) -> Res<PathBuf> {
    let vrt = cfg.work.join("global.vrt");
    if vrt.exists() {
        log!("global VRT present");
        return Ok(vrt);
    }

    let tiles = list.sorted();
    let jobs = cfg.jobs.max(1);
    if tiles.is_empty() {
        return Err("refusing to build a VRT over zero COGs".into());
    }
    // Row-aligned, never plain `chunks()`: a cut inside a 1-degree row is what
    // destroyed nine whole rows of the published archive. See [`shard_rows`].
    let shards: Vec<(usize, &[Cell])> = shard_rows(&tiles, jobs).into_iter().enumerate().collect();

    // The grid this mosaic exists to feed. The overview level is only correct
    // relative to it, so it is read from the same place the warp reads it.
    let target = crate::grid::extent(
        cfg.raster_global_maxzoom,
        -180.0,
        -squallar_geo::MERCATOR_LAT_LIMIT_DEG,
        180.0,
        squallar_geo::MERCATOR_LAT_LIMIT_DEG,
    );
    let overview = global_overview_level(target.nx);

    match overview {
        Some(level) => log!(
            "building global VRT over {} COGs (/vsis3/) in {} shards, {jobs} jobs, \
             at overview level {level} ({}x) for a {}px grid",
            tiles.len(),
            shards.len(),
            1u32 << (level + 1),
            target.nx
        ),
        None => log!(
            "building global VRT over {} COGs (/vsis3/) in {} shards, {jobs} jobs, \
             at FULL RESOLUTION -- the {}px grid is finer than any overview",
            tiles.len(),
            shards.len(),
            target.nx
        ),
    }

    let stage = cfg.work.join("vrt-shards");
    std::fs::create_dir_all(&stage)?;

    let shard_vrt = |i: usize| stage.join(format!("shard-{i:04}.vrt"));

    let failed = parallel(&shards, jobs, |(i, cells)| {
        let paths: String = cells
            .iter()
            .map(|c| format!("{}\n", cfg.tile_vsis3(&tile_name(*c))))
            .collect();
        let list_file = stage.join(format!("shard-{i:04}.txt"));
        std::fs::write(&list_file, paths)?;

        let mut args: Vec<String> = vec!["-q".into(), "-resolution".into(), "highest".into()];
        if let Some(level) = overview {
            args.push("-oo".into());
            args.push(format!("OVERVIEW_LEVEL={level}"));
        }
        args.push("-input_file_list".into());
        args.push(list_file.to_string_lossy().into_owned());
        args.push(shard_vrt(*i).to_string_lossy().into_owned());

        run(cmd("gdalbuildvrt", &args))
    });
    if failed > 0 {
        return Err(format!("{failed} of {} VRT shards failed", shards.len()).into());
    }

    // Every shard must have produced a file. A missing one here would otherwise
    // become a hole in the mosaic that reads as ocean -- silent, and invisible
    // until somebody looks at the terrain for that part of the world.
    let parts: Vec<String> = (0..shards.len())
        .map(|i| {
            let p = shard_vrt(i);
            if p.is_file() {
                Ok(p.to_string_lossy().into_owned())
            } else {
                Err(format!("VRT shard {i} reported success but wrote no file"))
            }
        })
        .collect::<Result<_, _>>()?;

    let combined_list = stage.join("shards.txt");
    std::fs::write(&combined_list, parts.join("\n") + "\n")?;
    run(cmd(
        "gdalbuildvrt",
        &[
            "-q",
            "-resolution",
            "highest",
            "-input_file_list",
            combined_list.to_string_lossy().as_ref(),
            vrt.to_string_lossy().as_ref(),
        ],
    ))?;

    Ok(vrt)
}

/// One global elevation raster, built ONCE from the COGs.
///
/// GDAL serves low-resolution reads out of each COG's own internal overviews,
/// so this touches a small fraction of the 1.5 TB.
fn build_global_elev(cfg: &Config, list: &TileList, out: &Path) -> Res<()> {
    if out.exists() {
        log!("global elevation raster present");
        return Ok(());
    }
    let vrt = build_global_vrt(cfg, list)?;

    let e = crate::grid::extent(
        cfg.raster_global_maxzoom,
        -180.0,
        -squallar_geo::MERCATOR_LAT_LIMIT_DEG,
        180.0,
        squallar_geo::MERCATOR_LAT_LIMIT_DEG,
    );
    log!(
        "global z{} grid: {}x{} px",
        cfg.raster_global_maxzoom,
        e.nx,
        e.ny
    );
    let mut c = warp(&vrt, out, &e, &["-co", "BIGTIFF=YES"]);
    c.args(["-multi", "-wo"])
        .arg(format!("NUM_THREADS={}", cfg.jobs));
    run(c)
}

/// The decimated scan grid [`assert_no_dead_land_rows`] reads: whole-mercator,
/// square like the mosaic, ~5.7 samples per degree each way at the equator
/// (rows grow poleward). At 1024 columns a 1-degree cell kept only 1-2 interior
/// columns and the ceil/floor window starved — half the cells could never read
/// dead. 2048 keeps at least a 4x4 interior everywhere between the polar
/// clamps, and the scan text stays a few tens of megabytes.
const STRIPE_SCAN_COLS: usize = 2048;
const STRIPE_SCAN_ROWS: usize = 2048;

/// How many consecutive dead land cells along one row trip the build.
///
/// The hazard this must not false-positive on is a run of listed cells whose
/// land the decimated scan can miss entirely: low atolls. Measured against the
/// pinned tile list, the longest adjacent run of pure-atoll cells in any row is
/// 7 (Tuamotus, S17–S19); the destroyed rows the tripwire exists for ran to
/// hundreds of cells. Twelve sits above the one with margin and far below the
/// other.
const STRIPE_RUN_CELLS: usize = 12;

/// FAIL the build if the global mosaic carries a dead row across land.
///
/// The symptom this pins down shipped once: whole 1-degree rows of the mosaic
/// zeroed by overlapping VRT shards (see [`shard_rows`]), which survived to the
/// published archive because nothing between `gdalwarp` and `pmtiles` ever
/// looked. This reads the mosaic back decimated — one `gdal_translate` to an
/// ASCII grid on stdout, no temp file — and refuses to continue when a long
/// run of consecutive land cells in one 1-degree row contains nothing but
/// (near-)zero samples. Costs one decompression pass over the mosaic, threaded
/// by the open option.
fn assert_no_dead_land_rows(list: &TileList, global: &Path) -> Res<()> {
    log!("scanning the global mosaic for dead land rows");
    let text = capture(cmd(
        "gdal_translate",
        &[
            "-q",
            "-of",
            "AAIGrid",
            "-outsize",
            &STRIPE_SCAN_COLS.to_string(),
            &STRIPE_SCAN_ROWS.to_string(),
            "-co",
            "DECIMAL_PRECISION=3",
            "-oo",
            "NUM_THREADS=ALL_CPUS",
            global.to_string_lossy().as_ref(),
            "/vsistdout/",
        ],
    ))?;
    let values = parse_aaigrid(&text, STRIPE_SCAN_COLS, STRIPE_SCAN_ROWS)?;
    let runs = dead_land_runs(list, STRIPE_SCAN_COLS, STRIPE_SCAN_ROWS, &values);
    if runs.is_empty() {
        log!("no dead land rows");
        return Ok(());
    }
    let shown: Vec<String> = runs
        .iter()
        .take(10)
        .map(|(lat, w, e)| format!("lat {lat}..{}, lon {w}..{}", lat + 1, e + 1))
        .collect();
    Err(format!(
        "the global mosaic holds {} dead land row segment(s) — real terrain \
         resampled to nothing. First {}:\n     {}\n     \
         This is the shard-clobber symptom; see shard_rows in raster.rs.",
        runs.len(),
        shown.len(),
        shown.join("\n     ")
    )
    .into())
}

/// Read exactly `ncols * nrows` values out of an Arc/Info ASCII grid.
///
/// Written against GDAL 3.13.3's actual `/vsistdout/` behaviour, which APPENDS
/// the `.prj` sidecar to the same stream after the data — so this stops after
/// the last value instead of reading to the end, and skips every line that
/// opens with a letter (the header rows, and that trailing `PROJCS[...]`).
fn parse_aaigrid(text: &str, ncols: usize, nrows: usize) -> Res<Vec<f32>> {
    let want = ncols * nrows;
    let mut values = Vec::with_capacity(want);
    let mut header_cols = None;
    let mut header_rows = None;
    for line in text.lines() {
        let mut fields = line.split_ascii_whitespace().peekable();
        let Some(first) = fields.peek() else { continue };
        if first.starts_with(|c: char| c.is_ascii_alphabetic()) {
            let key = fields.next().unwrap_or_default().to_ascii_lowercase();
            let value = fields.next().and_then(|v| v.parse::<usize>().ok());
            match key.as_str() {
                "ncols" => header_cols = value,
                "nrows" => header_rows = value,
                _ => {}
            }
            continue;
        }
        for field in fields {
            let v: f32 = field
                .parse()
                .map_err(|e| format!("mosaic scan value {field:?}: {e}"))?;
            values.push(v);
            if values.len() == want {
                break;
            }
        }
        if values.len() == want {
            break;
        }
    }
    if header_cols != Some(ncols) || header_rows != Some(nrows) {
        return Err(format!(
            "mosaic scan grid is {header_cols:?}x{header_rows:?}, not {ncols}x{nrows}"
        )
        .into());
    }
    if values.len() != want {
        return Err(format!("mosaic scan yielded {} of {want} values", values.len()).into());
    }
    Ok(values)
}

/// Every run of at least [`STRIPE_RUN_CELLS`] consecutive listed cells in one
/// 1-degree row whose every interior sample is (near-)zero, as
/// `(lat, west_lon, east_lon)` inclusive.
///
/// A cell too close to the mercator clamp to keep a 2x2 interior sample window
/// counts as alive, never dead: the scan cannot see it, and "cannot see" must
/// not trip a build. Pinned by `a_destroyed_row_trips_the_mosaic_scan` and
/// `a_short_dead_segment_stays_below_the_tripwire`.
fn dead_land_runs(
    list: &TileList,
    ncols: usize,
    nrows: usize,
    values: &[f32],
) -> Vec<(i32, i32, i32)> {
    let dead_cell = |lat: i32, lon: i32| -> bool {
        let c0 = (frac_tile_x(f64::from(lon), 0) * ncols as f64).ceil() as isize;
        let c1 = (frac_tile_x(f64::from(lon + 1), 0) * ncols as f64).floor() as isize;
        let r0 = (frac_tile_y(f64::from(lat + 1), 0) * nrows as f64).ceil() as isize;
        let r1 = (frac_tile_y(f64::from(lat), 0) * nrows as f64).floor() as isize;
        let (c0, r0) = (c0.max(0), r0.max(0));
        let (c1, r1) = (c1.min(ncols as isize), r1.min(nrows as isize));
        if c1 - c0 < 2 || r1 - r0 < 2 {
            return false;
        }
        (r0..r1).all(|r| (c0..c1).all(|c| values[r as usize * ncols + c as usize].abs() < 1e-3))
    };

    let mut runs = Vec::new();
    for lat in -90..90 {
        let mut start: Option<i32> = None;
        for lon in -180..=180 {
            let in_run = lon < 180 && list.contains(Cell { lat, lon }) && dead_cell(lat, lon);
            match (start, in_run) {
                (None, true) => start = Some(lon),
                (Some(w), false) => {
                    if (lon - w) as usize >= STRIPE_RUN_CELLS {
                        runs.push((lat, w, lon - 1));
                    }
                    start = None;
                }
                _ => {}
            }
        }
    }
    runs
}

/// `gdalwarp` onto one zoom's exact grid.
///
/// `-r average` on the ELEVATION, never on the encoded pixels; see the module
/// header.
fn warp(src: &Path, dst: &Path, e: &Extent, extra: &[&str]) -> std::process::Command {
    let mut c = cmd::<&str>("gdalwarp", &[]);
    c.args(["-q", "-overwrite", "-t_srs", "EPSG:3857", "-te"])
        .args([
            format!("{:.10}", e.xmin),
            format!("{:.10}", e.ymin),
            format!("{:.10}", e.xmax),
            format!("{:.10}", e.ymax),
        ])
        .arg("-ts")
        .args([e.nx.to_string(), e.ny.to_string()])
        // `-srcnodata 0 -dstnodata 0`: zero means "no data" from end to end.
        // The COGs declare no NODATA, so without `-srcnodata` a source zero is
        // a VALID pixel, and what becomes of it depends on the GDAL release:
        // 3.10.3 (the build box) writes it through as 0, which the declared
        // dstnodata makes invisible downstream — the shipped ocean behaviour —
        // while 3.13.3 (measured locally, 2026-08-29) nudges every valid
        // source zero to 1.4e-45 "to avoid being treated as NoData", which
        // hillshades the oceans opaque grey 181 instead of transparent 0.
        // Declaring source zeros AS nodata makes every release produce the
        // shipped behaviour, and keeps ocean zeros out of `-r average` at
        // coastlines instead of dragging shore pixels toward sea level.
        .args(["-r", "average", "-ot", "Float32"])
        .args(["-srcnodata", "0", "-dstnodata", "0"]);
    if dst.extension().is_some_and(|x| x == "tif") {
        c.args([
            "-co",
            "COMPRESS=DEFLATE",
            "-co",
            "PREDICTOR=3",
            "-co",
            "TILED=YES",
        ]);
    } else {
        // ENVI: a flat little-endian Float32 plane, header offset 0, which is
        // what the Terrain-RGB packer reads. Every GDAL raw driver is Create()-
        // based and needs a seekable output, so this cannot be a pipe.
        c.args(["-of", "ENVI"]);
    }
    c.args(extra).arg(src).arg(dst);
    c
}

#[allow(clippy::too_many_arguments)]
fn one_supercell(
    cfg: &Config,
    list: &TileList,
    z: u8,
    sc: &SuperCell,
    stage: &Path,
    global: &Path,
    target: &Path,
    lock: &Mutex<()>,
    elev_type_co: &[String],
) -> Res<()> {
    let done = stage.join(format!("{}.done", sc.name));
    if done.exists() {
        return Ok(());
    }
    let e = tile_extent(z, sc.range);
    let bbox = tile_bbox(z, sc.range);

    // Above the global zoom the source is a VRT over just the COGs this cell
    // overlaps, with the one-degree margin tile_bbox adds so the resampling
    // kernel and gdaldem's 3x3 window see real neighbours rather than an edge.
    let mut owned_vrt = None;
    let src: PathBuf = if z <= cfg.raster_global_maxzoom {
        global.to_path_buf()
    } else {
        let mut srcs = String::new();
        for lat in bbox.s.max(-90)..bbox.n.min(90) {
            for lon in bbox.w.max(-180)..bbox.e.min(180) {
                let cell = Cell { lat, lon };
                if list.contains(cell) {
                    srcs.push_str(&cfg.tile_vsis3(&tile_name(cell)));
                    srcs.push('\n');
                }
            }
        }
        if srcs.is_empty() {
            std::fs::File::create(&done)?;
            return Ok(());
        }
        let txt = stage.join(format!("{}.txt", sc.name));
        let vrt = stage.join(format!("{}.vrt", sc.name));
        std::fs::write(&txt, srcs)?;
        run(cmd(
            "gdalbuildvrt",
            &[
                "-q",
                "-input_file_list",
                txt.to_string_lossy().as_ref(),
                vrt.to_string_lossy().as_ref(),
            ],
        ))?;
        let _ = std::fs::remove_file(&txt);
        owned_vrt = Some(vrt.clone());
        vrt
    };

    let elev = stage.join(match cfg.encoding {
        Encoding::Hillshade => format!("{}.elev.tif", sc.name),
        Encoding::TerrainRgb => format!("{}.elev.img", sc.name),
    });
    let part = stage.join(format!("{}.mbtiles", sc.name));
    let result = encode_and_tile(cfg, z, sc, &e, bbox.clat, &src, &elev, &part, elev_type_co);

    if let Some(v) = owned_vrt {
        let _ = std::fs::remove_file(v);
    }
    let _ = std::fs::remove_file(&elev);
    let _ = std::fs::remove_file(elev.with_extension("hdr"));

    match result {
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        Ok(true) => {
            let _guard = lock.lock().map_err(|_| "accumulator lock poisoned")?;
            mbtiles::merge(&part, target)?;
        }
        Ok(false) => {}
    }
    let _ = std::fs::remove_file(&part);
    std::fs::File::create(&done)?;
    Ok(())
}

/// Warp, encode and tile one super-cell. `Ok(false)` means the cell produced no
/// tiles, which is expected for open ocean.
#[allow(clippy::too_many_arguments)]
fn encode_and_tile(
    cfg: &Config,
    z: u8,
    sc: &SuperCell,
    e: &Extent,
    clat: f64,
    src: &Path,
    elev: &Path,
    part: &Path,
    elev_type_co: &[String],
) -> Res<bool> {
    run(warp(src, elev, e, &[]))?;

    let encoded: PathBuf = match cfg.encoding {
        Encoding::Hillshade => {
            let out = elev.with_extension("hs.tif");
            // -s corrects for the fact that EPSG:3857 "metres" are not ground
            // metres: they are inflated by 1/cos(lat), so a slope computed
            // against raw Mercator pixel spacing is too shallow by cos(lat) — a
            // factor of 2 at 60N, which is the difference between the Alps
            // looking like the Alps and looking like a rumpled sheet. gdaldem
            // takes one scalar, so this uses the super-cell's centre latitude;
            // a super-cell spans little latitude at the zooms where relief is
            // legible, so the residual is small.
            //
            // -compute_edges stops a one-pixel dark frame appearing at every
            // super-cell edge, which would otherwise draw a grid over the planet.
            let scale = clat.to_radians().cos().max(1e-6);
            run(cmd(
                "gdaldem",
                &[
                    "hillshade",
                    "-q",
                    "-alg",
                    "Horn",
                    "-z",
                    "1",
                    "-s",
                    &format!("{scale:.10}"),
                    "-az",
                    "315",
                    "-alt",
                    "45",
                    "-compute_edges",
                    "-co",
                    "COMPRESS=DEFLATE",
                    elev.to_string_lossy().as_ref(),
                    out.to_string_lossy().as_ref(),
                ],
            ))?;
            out
        }
        Encoding::TerrainRgb => pack_terrain_rgb(e, elev)?,
    };

    // No -co ZOOM_LEVEL: the MBTiles driver advertises it but its CreateCopy
    // path rejects it. It is unnecessary anyway — the source was warped onto
    // zoom z's exact grid, so the driver derives z from the resolution. That
    // inference is asserted rather than assumed, because a silently-misplaced
    // zoom would put real tiles at the wrong address and still look like a
    // successful build.
    let mut args: Vec<String> = vec![
        "-q".into(),
        "-of".into(),
        "MBTILES".into(),
        "-co".into(),
        format!("TILE_FORMAT={}", cfg.tile_format),
    ];
    // `QUALITY`, and it was `WEBP_LEVEL` until a real build printed
    //
    //   Warning 6: driver MBTiles does not support creation option WEBP_LEVEL
    //
    // once per super-cell for two hours. **`WEBP_LEVEL` is a GTiff option; the
    // MBTiles driver's is `QUALITY`** ("Quality for JPEG and WEBP tiles",
    // default 75), so the setting was being dropped and every tile written at
    // the default -- while the comment here claimed 85 and the size figures
    // elsewhere were quoted as measured at 85.
    //
    // The old comment is worth keeping as evidence rather than deleting: it
    // asserted a configuration the code could not produce, and nothing caught
    // that except a warning nobody was reading. GDAL does not fail on an
    // unknown creation option, so there is no gate here to add -- only the
    // habit of reading what the tool prints.
    //
    // 85 rather than the default because a hillshade is smooth grey gradient,
    // which is where lossy banding shows. Worth knowing before treating that as
    // urgent: the archive built at the dropped default was inspected and does
    // NOT visibly band -- 185 distinct grey levels in a 256x256 z12 tile over
    // Colorado -- so this is fidelity we intended, not damage we shipped.
    if cfg.tile_format.eq_ignore_ascii_case("WEBP") {
        args.push("-co".into());
        args.push("QUALITY=85".into());
    }
    args.extend(elev_type_co.iter().cloned());
    args.push(encoded.to_string_lossy().into_owned());
    args.push(part.to_string_lossy().into_owned());
    let translate = run(cmd("gdal_translate", &args));

    let _ = std::fs::remove_file(&encoded);
    if cfg.encoding == Encoding::TerrainRgb {
        let _ = std::fs::remove_file(encoded.with_extension("bin"));
    }
    translate?;

    let zooms = mbtiles::zoom_levels(part)?;
    if zooms.is_empty() {
        // A super-cell that is entirely ocean or entirely outside the DEM's
        // coverage produces no tiles. That is expected — the land mask is per
        // degree cell, so a cell can qualify on one corner and still be empty
        // at tile resolution.
        return Ok(false);
    }
    if zooms != [z] {
        return Err(format!("{}: warped to z{z} but MBTiles holds z{zooms:?}", sc.name).into());
    }
    Ok(true)
}

/// Pack a raw Float32 plane to interleaved Terrain-RGB, and wrap it in a VRT
/// GDAL can tile.
///
/// The VRT names the raw file by BASENAME with `relativeToVRT="1"`.
/// `VRTRawRasterBand` refuses an absolute path unless
/// `GDAL_VRT_RAWRASTERBAND_ALLOWED_SOURCE` is set, defaulting to
/// sibling-or-child of the VRT — measured on GDAL 3.13.3, which errors with
/// "is invalid because the relativeToVRT flag is not set".
pub fn pack_terrain_rgb(e: &Extent, elev: &Path) -> Res<PathBuf> {
    let raw = elev.with_extension("rgb.bin");
    let vrt = elev.with_extension("rgb.vrt");
    let pixels = u64::from(e.nx) * u64::from(e.ny);

    let got = std::fs::metadata(elev)?.len();
    if got != pixels * 4 {
        return Err(format!(
            "{} is {got} bytes; {}x{} Float32 is {} bytes",
            elev.display(),
            e.nx,
            e.ny,
            pixels * 4
        )
        .into());
    }
    {
        let mut src = BufReader::new(std::fs::File::open(elev)?);
        let mut dst = BufWriter::new(std::fs::File::create(&raw)?);
        trgb::pack_stream(&mut src, &mut dst, pixels)?;
        std::io::Write::flush(&mut dst)?;
    }

    let base = raw
        .file_name()
        .ok_or("packed raster has no file name")?
        .to_string_lossy()
        .into_owned();
    let resx = (e.xmax - e.xmin) / f64::from(e.nx);
    let resy = (e.ymax - e.ymin) / f64::from(e.ny);
    let line = u64::from(e.nx) * 3;
    let mut xml = format!(
        "<VRTDataset rasterXSize=\"{}\" rasterYSize=\"{}\">\n  \
         <SRS>EPSG:3857</SRS>\n  \
         <GeoTransform>{:.12}, {:.12}, 0, {:.12}, 0, -{:.12}</GeoTransform>\n",
        e.nx, e.ny, e.xmin, resx, e.ymax, resy
    );
    for (band, (offset, interp)) in [(0, "Red"), (1, "Green"), (2, "Blue")].iter().enumerate() {
        xml.push_str(&format!(
            "  <VRTRasterBand dataType=\"Byte\" band=\"{}\" subClass=\"VRTRawRasterBand\">\n    \
             <ColorInterp>{interp}</ColorInterp>\n    \
             <SourceFilename relativeToVRT=\"1\">{base}</SourceFilename>\n    \
             <ImageOffset>{offset}</ImageOffset>\n    \
             <PixelOffset>3</PixelOffset>\n    \
             <LineOffset>{line}</LineOffset>\n  \
             </VRTRasterBand>\n",
            band + 1
        ));
    }
    xml.push_str("</VRTDataset>\n");
    std::fs::write(&vrt, xml)?;
    Ok(vrt)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A believable latitude-sorted planet: rows of varying width, some short
    /// (islands), some wide (continents), in exactly the order
    /// `TileList::sorted` produces.
    fn synthetic_planet() -> Vec<Cell> {
        let mut cells = Vec::new();
        for lat in -85..85 {
            // Deterministically varied widths, 3..=180 cells per row.
            let width = 3 + (i64::from(lat) * 37).rem_euclid(178) as i32;
            let start = -90 + (i64::from(lat) * 13).rem_euclid(60) as i32;
            for lon in start..start + width {
                cells.push(Cell { lat, lon });
            }
        }
        cells
    }

    /// **Every COG lands in exactly one shard, and there are never more shards
    /// than jobs.** A tile silently dropped here is not a crash: it is a hole in
    /// the global mosaic that resamples as ocean, which nobody sees until they
    /// look at the terrain for that part of the world.
    #[test]
    fn row_aligned_shards_cover_every_cog_exactly_once() {
        let planet = synthetic_planet();
        let one_row: Vec<Cell> = (-180..180).map(|lon| Cell { lat: 7, lon }).collect();
        let tiny = vec![Cell { lat: 0, lon: 0 }];
        for tiles in [&planet, &one_row, &tiny] {
            for jobs in [1_usize, 4, 48, 64] {
                let shards = shard_rows(tiles, jobs);
                assert!(
                    shards.len() <= jobs,
                    "{} COGs over {jobs} jobs made {} shards",
                    tiles.len(),
                    shards.len()
                );
                let flat: Vec<Cell> = shards.concat();
                assert_eq!(
                    &flat,
                    tiles,
                    "{} COGs over {jobs} jobs did not reassemble in order",
                    tiles.len()
                );
            }
        }
    }

    /// **The invariant the stripe fix rests on: consecutive shards' extents are
    /// disjoint latitude bands.** A split row is the one shape whose bounding
    /// boxes overlap, and an overlap is where a later shard's implicit zeros
    /// clobber an earlier shard's real terrain — see [`shard_rows`].
    #[test]
    fn no_latitude_row_is_ever_split_across_shards() {
        let planet = synthetic_planet();
        for jobs in [2_usize, 4, 48, 64] {
            let shards = shard_rows(&planet, jobs);
            for pair in shards.windows(2) {
                let last = pair[0].last().unwrap();
                let first = pair[1].first().unwrap();
                assert!(
                    last.lat < first.lat,
                    "{jobs} jobs: a shard ends at {last:?} and the next starts \
                     at {first:?} — the same row, in two shards"
                );
            }
            // NON-VACUITY: plain ceiling-division chunking of this same input
            // WOULD split a row at some boundary, so the assertion above can
            // fail on the code this replaced.
            if jobs > 1 {
                let size = shard_size(planet.len(), jobs);
                let naive_splits = planet
                    .chunks(size)
                    .zip(planet.chunks(size).skip(1))
                    .filter(|(a, b)| a.last().unwrap().lat == b.first().unwrap().lat)
                    .count();
                assert!(
                    naive_splits > 0,
                    "{jobs} jobs: the naive chunking splits no row, so this \
                     input cannot distinguish the fix from the defect"
                );
            }
        }
    }

    /// The real call: 26,450 COGs across the build box's 48 vCPUs.
    ///
    /// Pinned as a number because the whole point of the change is the ratio.
    /// One shard would be the eight-hour serial build this replaced, so a
    /// regression that quietly stopped sharding would otherwise only show up as
    /// a slow run nobody attributes.
    #[test]
    fn the_planet_shards_across_every_core() {
        let size = shard_size(26_450, 48);
        assert_eq!(size, 552);
        assert_eq!(26_450_usize.div_ceil(size), 48, "shards");
        // NON-VACUITY: the unsharded build is what this must not collapse back
        // to, and it is a different number.
        assert_ne!(size, 26_450);
    }

    /// **The global z8 grid picks the 8x overview, and that is the whole fix.**
    ///
    /// Pinned as concrete numbers because the failure it prevents was measured
    /// and expensive: without an overview the warp read near-full-resolution
    /// pixels, 240 GB in 3.75 hours, to fill a grid that cannot hold them.
    #[test]
    fn the_global_grid_reads_the_deepest_overview_that_is_still_finer() {
        // z8 global: 256 * 2^8 = 65,536 px.
        let level = global_overview_level(65_536).expect("z8 has a usable overview");
        assert_eq!(level, 2, "the 8x level");

        let mosaic = (360 * GLO30_PX_PER_DEGREE) / (1 << (level + 1));
        assert_eq!(mosaic, 162_000);
        assert!(
            mosaic >= 65_536,
            "the chosen overview must not be coarser than the grid it feeds"
        );

        // NON-VACUITY, and it has to read the FUNCTION's answer rather than
        // compare two literals -- the first spelling of this was
        // `1_296_000 > 19 * 65_536`, which clippy correctly called an assertion
        // that is always true. This one fails if the chosen level ever collapses
        // to a token reduction.
        let full = 360 * GLO30_PX_PER_DEGREE;
        assert!(
            mosaic * 7 < full,
            "the chosen overview reduces {full} to {mosaic}, which is not the \
             large reduction this exists for"
        );
    }

    /// **It steps back rather than upsampling**, at every level.
    ///
    /// One level too deep hands the warp a mosaic coarser than its target, which
    /// invents detail instead of skipping it. Walked across the whole range so
    /// the boundary is pinned from both sides rather than at one point.
    #[test]
    fn a_finer_grid_forces_a_shallower_overview() {
        for level in 0..GLO30_OVERVIEW_LEVELS {
            let mosaic = (360 * GLO30_PX_PER_DEGREE) / (1 << (level + 1));
            // Exactly at the mosaic width, this level still fits.
            assert_eq!(
                global_overview_level(mosaic),
                Some(level),
                "at {mosaic}px the {level} level is exactly finest-that-fits"
            );
            // One pixel finer, it must not be chosen.
            let chosen = global_overview_level(mosaic + 1);
            assert!(
                chosen.is_none() || chosen.unwrap() < level,
                "at {}px it kept level {level}, which is coarser than the target",
                mosaic + 1
            );
        }
    }

    /// A grid finer than every overview reads full resolution rather than
    /// quietly picking the least-bad one.
    #[test]
    fn a_grid_finer_than_every_overview_reads_full_resolution() {
        assert_eq!(global_overview_level(1_296_000), None);
        assert_eq!(global_overview_level(u32::MAX), None);
        // And the coarsest useful case still resolves.
        assert_eq!(global_overview_level(1), Some(GLO30_OVERVIEW_LEVELS - 1));
    }

    /// `chunks(0)` panics, so the floor is not cosmetic.
    #[test]
    fn a_shard_is_never_empty() {
        assert_eq!(shard_size(0, 48), 1);
        assert_eq!(shard_size(10, 0), 10);
    }

    /// A tile list of exactly the cells `lats x lons`, via the same parser the
    /// real list goes through.
    fn scan_list(lats: std::ops::Range<i32>, lons: std::ops::Range<i32>) -> TileList {
        let mut text = String::new();
        for lat in lats {
            for lon in lons.clone() {
                text.push_str(&tile_name(Cell { lat, lon }));
                text.push_str("\r\n");
            }
        }
        TileList::parse(text.as_bytes()).expect("synthetic tile list parses")
    }

    /// Zero every scan pixel of the 1-degree band `lat..lat+1` across
    /// `lons`, bounds widened outward so no interior sample survives.
    fn kill_band(values: &mut [f32], lat: i32, lons: std::ops::Range<i32>) {
        let ncols = STRIPE_SCAN_COLS;
        let c0 = (frac_tile_x(f64::from(lons.start), 0) * ncols as f64).floor() as usize;
        let c1 = (frac_tile_x(f64::from(lons.end), 0) * ncols as f64).ceil() as usize;
        let r0 = (frac_tile_y(f64::from(lat + 1), 0) * STRIPE_SCAN_ROWS as f64).floor() as usize;
        let r1 = (frac_tile_y(f64::from(lat), 0) * STRIPE_SCAN_ROWS as f64).ceil() as usize;
        for r in r0..r1 {
            for value in &mut values[r * ncols + c0..r * ncols + c1] {
                *value = 0.0;
            }
        }
    }

    /// **A destroyed row across land fails the build; a clean mosaic passes.**
    /// The shape is the shipped defect exactly: one 1-degree band of a listed
    /// land row reading zero while its neighbours hold terrain.
    #[test]
    fn a_destroyed_row_trips_the_mosaic_scan() {
        let list = scan_list(40..45, -20..30);
        let live = vec![100.0_f32; STRIPE_SCAN_COLS * STRIPE_SCAN_ROWS];

        // NON-VACUITY: the clean mosaic reports nothing.
        assert_eq!(
            dead_land_runs(&list, STRIPE_SCAN_COLS, STRIPE_SCAN_ROWS, &live),
            []
        );

        let mut destroyed = live.clone();
        kill_band(&mut destroyed, 41, -20..11);
        let runs = dead_land_runs(&list, STRIPE_SCAN_COLS, STRIPE_SCAN_ROWS, &destroyed);
        assert_eq!(runs, [(41, -20, 10)], "31 dead cells at N41 must trip");
    }

    /// Below [`STRIPE_RUN_CELLS`] the scan stays quiet — that headroom is what
    /// keeps a run of low atolls the decimated scan cannot see (measured
    /// longest: 7, Tuamotus) from failing a healthy build. At the threshold it
    /// trips, so the boundary is pinned from both sides.
    #[test]
    fn a_short_dead_segment_stays_below_the_tripwire() {
        let list = scan_list(40..45, -20..30);
        let live = vec![100.0_f32; STRIPE_SCAN_COLS * STRIPE_SCAN_ROWS];

        let mut short = live.clone();
        kill_band(&mut short, 41, -20..-20 + STRIPE_RUN_CELLS as i32 - 1);
        assert_eq!(
            dead_land_runs(&list, STRIPE_SCAN_COLS, STRIPE_SCAN_ROWS, &short),
            [],
            "one below the threshold must not trip"
        );

        let mut exact = live;
        kill_band(&mut exact, 41, -20..-20 + STRIPE_RUN_CELLS as i32);
        assert_eq!(
            dead_land_runs(&list, STRIPE_SCAN_COLS, STRIPE_SCAN_ROWS, &exact),
            [(41, -20, -20 + STRIPE_RUN_CELLS as i32 - 1)],
            "exactly the threshold must trip"
        );
    }

    /// The parse reads exactly `ncols x nrows` values and no further. GDAL
    /// 3.13.3 appends the `.prj` sidecar to the same `/vsistdout/` stream, so
    /// reading to the end would choke on `PROJCS[...]`.
    #[test]
    fn the_aaigrid_parse_stops_before_the_appended_prj() {
        let text = "ncols        3\nnrows        2\nxllcorner    -1.0\nyllcorner    -2.0\n\
                    dx           1.0\ndy           2.0\nNODATA_value 0.000\n\
                    1.5 0.000 -430.0 \n2.5 3.5 4.5 \n\
                    PROJCS[\"WGS_1984_Web_Mercator_Auxiliary_Sphere\"]\n";
        let values = parse_aaigrid(text, 3, 2).expect("parses");
        assert_eq!(values, [1.5, 0.0, -430.0, 2.5, 3.5, 4.5]);

        // A grid of the wrong shape is an error, not a truncation.
        assert!(parse_aaigrid(text, 4, 2).is_err());
    }
}
