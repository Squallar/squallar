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
use crate::grid::{Extent, tile_bbox, tile_extent};
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
    } else {
        log!(
            "min zoom {} > {}: skipping global raster",
            cfg.raster_minzoom,
            cfg.raster_global_maxzoom
        );
    }

    // Fewer jobs than cores: each warp is already multi-threaded internally and
    // each holds ~1 GB of Float32 plus its encoded copy.
    let sc_jobs = (cfg.jobs / 4).max(1);
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

/// One global elevation raster, built ONCE from the COGs.
///
/// GDAL serves low-resolution reads out of each COG's own internal overviews,
/// so this touches a small fraction of the 1.5 TB.
fn build_global_elev(cfg: &Config, list: &TileList, out: &Path) -> Res<()> {
    if out.exists() {
        log!("global elevation raster present");
        return Ok(());
    }
    log!("building global VRT over {} COGs (/vsis3/)", list.len());
    let paths: String = list
        .sorted()
        .into_iter()
        .map(|c| format!("{}\n", cfg.tile_vsis3(&tile_name(c))))
        .collect();
    let list_file = cfg.work.join("vsis3-tiles.txt");
    std::fs::write(&list_file, paths)?;
    let vrt = cfg.work.join("global.vrt");
    run(cmd(
        "gdalbuildvrt",
        &[
            "-q",
            "-input_file_list",
            list_file.to_string_lossy().as_ref(),
            vrt.to_string_lossy().as_ref(),
        ],
    ))?;

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
        .args(["-r", "average", "-ot", "Float32", "-dstnodata", "0"]);
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
