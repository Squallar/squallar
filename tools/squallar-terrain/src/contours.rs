//! The CONTOUR archive: MVT vector tiles, PMTiles v3.
//!
//! Chunked so peak disk is bounded by one chunk in flight per job rather than
//! by the whole planet. Intermediates are deleted as soon as the chunk they
//! belong to has been tiled.

use std::path::{Path, PathBuf};

use crate::config::{ATTRIBUTION, BANDS, CONTOUR_ATTR, CONTOUR_LAYER, Config};
use crate::run::{Pipeline, cmd, need, parallel, run};
use crate::tiles::{Cell, Chunk, TileList, chunks, tile_name};
use crate::{Res, log, pmtiles};

pub fn build(cfg: &Config, list: &TileList) -> Res<()> {
    need(&["curl", "gdal_contour", "tippecanoe", "tile-join"])?;

    let parts = cfg.work.join("contour-parts");
    let stage = cfg.tmp.join("contour-stage");
    std::fs::create_dir_all(&cfg.out)?;
    std::fs::create_dir_all(&parts)?;
    std::fs::create_dir_all(&stage)?;

    log!("enumerating {}-degree chunks", cfg.chunk_deg);
    let mut work = chunks(list, cfg.chunk_deg);
    if let Some(f) = &cfg.only_chunk {
        work.retain(|c| c.name.contains(f.as_str()));
        log!("ONLY_CHUNK={f} -> {} chunks", work.len());
    }
    log!("{} populated chunks, {} jobs", work.len(), cfg.jobs);

    let failed = parallel(&work, cfg.jobs, |chunk| {
        one_chunk(cfg, list, chunk, &parts, &stage)
    });
    if failed > 0 {
        return Err(format!(
            "{failed} of {} contour chunks failed. The parts already built are kept; \
             rerunning resumes from them.",
            work.len()
        )
        .into());
    }

    join_generations(cfg, &parts, &cfg.contours_pmtiles())?;
    pmtiles::assert_archive(&cfg.contours_pmtiles())?;
    log!(
        "contours: {} ({} bytes)",
        cfg.contours_pmtiles().display(),
        std::fs::metadata(cfg.contours_pmtiles())?.len()
    );
    Ok(())
}

/// One chunk. Stages its degree tiles, streams contours straight into
/// tippecanoe without ever writing GeoJSON to disk, joins the zoom bands, and
/// drops everything it staged.
fn one_chunk(cfg: &Config, list: &TileList, chunk: &Chunk, parts: &Path, stage: &Path) -> Res<()> {
    let out = parts.join(format!("{}.pmtiles", chunk.name));
    if out.exists() {
        log!("{}: already built, skipping", chunk.name);
        return Ok(());
    }

    let cells: Vec<Cell> = (chunk.s..chunk.n)
        .flat_map(|lat| (chunk.w..chunk.e).map(move |lon| Cell { lat, lon }))
        .filter(|c| list.contains(*c))
        .collect();
    if cells.is_empty() {
        log!("{}: no land tiles", chunk.name);
        return Ok(());
    }

    let dir = stage.join(&chunk.name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let result = contour_chunk(cfg, chunk, &cells, &dir, &out);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn contour_chunk(cfg: &Config, chunk: &Chunk, cells: &[Cell], dir: &Path, out: &Path) -> Res<()> {
    log!("{}: staging {} DEM tiles", chunk.name, cells.len());
    let mut staged = Vec::with_capacity(cells.len());
    for cell in cells {
        let name = tile_name(*cell);
        let path = dir.join(format!("{name}.tif"));
        run(cmd(
            "curl",
            &[
                "-fsS",
                "--retry",
                "5",
                "--retry-delay",
                "2",
                "-o",
                path.to_string_lossy().as_ref(),
                &cfg.tile_url(&name),
            ],
        ))?;
        staged.push(path);
    }

    let mut band_parts: Vec<PathBuf> = Vec::new();
    for band in BANDS {
        let bout = dir.join(format!("band_{}_{}.pmtiles", band.lo, band.hi));
        log!(
            "{}: contouring at {} m for z{}-z{}",
            chunk.name,
            band.interval,
            band.lo,
            band.hi
        );

        // gdal_contour writes GeoJSONSeq to stdout and the whole chunk is one
        // stream, so no GeoJSON ever lands on disk. Measured on N39 W106: the
        // two-step GPKG + GeoJSON route costs 66.7 MB per degree tile at 100 m,
        // this costs 0.
        let mut pipe = Pipeline::to(cmd(
            "tippecanoe",
            &[
                "-q",
                "--force",
                &format!("-Z{}", band.lo),
                &format!("-z{}", band.hi),
                "-l",
                CONTOUR_LAYER,
                "-y",
                CONTOUR_ATTR,
                "--no-feature-limit",
                "--no-tile-size-limit",
                "--attribution",
                ATTRIBUTION,
                "-t",
                dir.to_string_lossy().as_ref(),
                "-o",
                bout.to_string_lossy().as_ref(),
            ],
        ))?;
        for tif in &staged {
            pipe.feed(cmd(
                "gdal_contour",
                &[
                    "-q",
                    "-a",
                    CONTOUR_ATTR,
                    "-i",
                    &band.interval.to_string(),
                    "-f",
                    "GeoJSONSeq",
                    tif.to_string_lossy().as_ref(),
                    "/vsistdout/",
                ],
            ))?;
        }
        let result = pipe.finish()?;

        if result.status.success() {
            band_parts.push(bout);
            continue;
        }
        // A coarse band over low relief legitimately produces NOTHING: a chunk
        // whose highest ground is 300 m has no 1000 m contour, and tippecanoe
        // exits non-zero with "Did not read any valid geometries". That is
        // data, not failure — roughly flat land is most of the planet's land.
        //
        // Reaching this branch means every gdal_contour above already exited 0,
        // which is what makes the message trustworthy. A shell pipeline cannot
        // establish that: if gdal_contour dies, tippecanoe reads nothing and
        // prints exactly this, and the chunk is recorded as flat.
        if result.stderr.contains("Did not read any valid geometries") {
            log!(
                "{}: no features at {} m, band z{}-z{} omitted",
                chunk.name,
                band.interval,
                band.lo,
                band.hi
            );
            let _ = std::fs::remove_file(&bout);
            continue;
        }
        return Err(format!(
            "{}: tippecanoe failed at {} m:\n{}",
            chunk.name, band.interval, result.stderr
        )
        .into());
    }

    if band_parts.is_empty() {
        log!("{}: no contours at any interval", chunk.name);
        return Ok(());
    }

    // The temp name MUST keep the .pmtiles suffix; see pmtiles::assert_archive.
    let tmp = out.with_extension("tmp.pmtiles");
    let mut join = vec!["-q".to_string(), "--force".into(), "-o".into()];
    join.push(tmp.to_string_lossy().into_owned());
    join.extend(band_parts.iter().map(|p| p.to_string_lossy().into_owned()));
    run(cmd("tile-join", &join))?;
    pmtiles::assert_archive(&tmp)?;
    std::fs::rename(&tmp, out)?;
    log!("{}: {} bytes", chunk.name, std::fs::metadata(out)?.len());
    Ok(())
}

/// Join in generations, because thousands of paths on one command line is both
/// an ARG_MAX hazard and a memory one.
fn join_generations(cfg: &Config, parts: &Path, final_out: &Path) -> Res<()> {
    let mut gen_dir = parts.to_path_buf();
    let mut round = 0;
    loop {
        let mut archives = list_archives(&gen_dir)?;
        archives.sort();
        match archives.len() {
            0 => return Err(format!("no contour archives in {}", gen_dir.display()).into()),
            1 => {
                std::fs::create_dir_all(final_out.parent().unwrap_or(Path::new(".")))?;
                std::fs::rename(&archives[0], final_out)?;
                if gen_dir != parts {
                    let _ = std::fs::remove_dir_all(&gen_dir);
                }
                return Ok(());
            }
            n => log!("join round {}: {n} archives", round + 1),
        }

        round += 1;
        let next = cfg.work.join(format!("join-{round}"));
        let _ = std::fs::remove_dir_all(&next);
        std::fs::create_dir_all(&next)?;

        let groups: Vec<(usize, Vec<PathBuf>)> = archives
            .chunks(16)
            .enumerate()
            .map(|(i, g)| (i, g.to_vec()))
            .collect();
        let failed = parallel(&groups, cfg.jobs, |(i, group)| {
            let out = next.join(format!("{i:06}.pmtiles"));
            let mut args = vec!["-q".to_string(), "--force".into(), "-o".into()];
            args.push(out.to_string_lossy().into_owned());
            args.extend(group.iter().map(|p| p.to_string_lossy().into_owned()));
            run(cmd("tile-join", &args))?;
            pmtiles::assert_archive(&out)
        });
        if failed > 0 {
            return Err(format!("{failed} tile-join groups failed in round {round}").into());
        }
        if gen_dir != parts {
            let _ = std::fs::remove_dir_all(&gen_dir);
        }
        gen_dir = next;
    }
}

fn list_archives(dir: &Path) -> Res<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        if p.extension().is_some_and(|e| e == "pmtiles") {
            out.push(p);
        }
    }
    Ok(out)
}
