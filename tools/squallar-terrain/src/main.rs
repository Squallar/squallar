use std::path::Path;
use std::process::ExitCode;

use squallar_terrain::config::{BANDS, Config, DEM_BUCKET_REGION, TILELIST};
use squallar_terrain::grid;
use squallar_terrain::run::{cmd, need, run};
use squallar_terrain::tiles::{TileList, chunks, supercells};
use squallar_terrain::{Res, contours, log, logging, raster};

const USAGE: &str = "\
squallar-terrain — Copernicus GLO-30 contour and terrain archives

  build [all|contours|raster]    the whole job (default)

Grid arithmetic, for inspection:

  extent      Z W S E N          -> xmin ymin xmax ymax nx ny   (EPSG:3857 m)
  tile-extent Z TX0 TY0 TX1 TY1  -> xmin ymin xmax ymax nx ny
  bbox        Z TX0 TY0 TX1 TY1  -> w s e n clat                (whole degrees)
  chunks      DEG                -> W S E N name  per populated cell
  supercells  Z SIDE             -> TX0 TY0 TX1 TY1 name  per land block

`chunks` and `supercells` read tileList.txt on stdin.

Environment: WORK OUT TMP JOBS DEM_BUCKET RASTER_ENCODING RASTER_MINZOOM
RASTER_MAXZOOM RASTER_TILE_FORMAT RASTER_GLOBAL_MAXZOOM RASTER_BBOX CHUNK_DEG
SUPERCELL ONLY_CHUNK ONLY_SUPERCELL

RASTER_BBOX=west,south,east,north in degrees clips the raster pass to a region,
rounded outward to whole super-cells. RASTER_BBOX=-125,24,-66,50 is CONUS.
";

fn main() -> ExitCode {
    logging::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    match dispatch(&argv) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[FATAL] {e}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(argv: &[&str]) -> Res<()> {
    match argv {
        [] | ["build"] => build("all"),
        ["build", what] => build(what),
        ["extent", z, w, s, e, n] => {
            println!(
                "{}",
                grid::extent(num(z)?, num(w)?, num(s)?, num(e)?, num(n)?).line()
            );
            Ok(())
        }
        ["tile-extent", z, tx0, ty0, tx1, ty1] => {
            println!(
                "{}",
                grid::tile_extent(num(z)?, range(tx0, ty0, tx1, ty1)?).line()
            );
            Ok(())
        }
        ["bbox", z, tx0, ty0, tx1, ty1] => {
            println!(
                "{}",
                grid::tile_bbox(num(z)?, range(tx0, ty0, tx1, ty1)?).line()
            );
            Ok(())
        }
        ["chunks", deg] => {
            for c in chunks(&read_list()?, num(deg)?) {
                println!("{} {} {} {} {}", c.w, c.s, c.e, c.n, c.name);
            }
            Ok(())
        }
        ["supercells", z, side] => {
            for sc in supercells(&read_list()?, num(z)?, num(side)?) {
                let r = sc.range;
                println!("{} {} {} {} {}", r.tx0, r.ty0, r.tx1, r.ty1, sc.name);
            }
            Ok(())
        }
        ["-h" | "--help" | "help"] => {
            print!("{USAGE}");
            Ok(())
        }
        _ => Err(format!("unrecognised arguments: {}\n\n{USAGE}", argv.join(" ")).into()),
    }
}

fn num<T: std::str::FromStr>(s: &str) -> Res<T> {
    s.parse()
        .map_err(|_| format!("{s:?} is not a number").into())
}

fn range(tx0: &str, ty0: &str, tx1: &str, ty1: &str) -> Res<grid::TileRange> {
    Ok(grid::TileRange {
        tx0: num(tx0)?,
        ty0: num(ty0)?,
        tx1: num(tx1)?,
        ty1: num(ty1)?,
    })
}

/// The grid subcommands read the raw list on stdin and do NOT check the pin;
/// they are inspection, and a pin failure there would be noise.
fn read_list() -> Res<TileList> {
    let mut raw = Vec::new();
    std::io::Read::read_to_end(&mut std::io::stdin().lock(), &mut raw)?;
    TileList::parse(&raw)
}

fn build(what: &str) -> Res<()> {
    let cfg = Config::from_env()?;
    std::fs::create_dir_all(&cfg.work)?;
    std::fs::create_dir_all(&cfg.out)?;
    std::fs::create_dir_all(&cfg.tmp)?;

    log!("squallar terrain build");
    log!(
        "  DEM      {}  (s3://{}, {DEM_BUCKET_REGION})",
        TILELIST.release,
        cfg.bucket
    );
    log!("  work     {}", cfg.work.display());
    log!("  out      {}", cfg.out.display());
    log!("  jobs     {}", cfg.jobs);
    for b in BANDS {
        log!("  contours z{}-z{} at {} m", b.lo, b.hi, b.interval);
    }
    log!(
        "  raster   {} z{}-z{} {}",
        cfg.encoding.as_str(),
        cfg.raster_minzoom,
        cfg.raster_maxzoom,
        cfg.tile_format
    );

    // Fetched once and shared, so the pin is checked once and both passes agree
    // on which tiles exist.
    let list = fetch_tilelist(&cfg)?;

    match what {
        "all" => {
            contours::build(&cfg, &list)?;
            raster::build(&cfg, &list)?;
        }
        "contours" => contours::build(&cfg, &list)?,
        "raster" => raster::build(&cfg, &list)?,
        other => return Err(format!("usage: build [all|contours|raster], not {other:?}").into()),
    }

    log!("done");
    for entry in std::fs::read_dir(&cfg.out)? {
        let p = entry?.path();
        log!("  {} ({} bytes)", p.display(), std::fs::metadata(&p)?.len());
    }
    Ok(())
}

fn fetch_tilelist(cfg: &Config) -> Res<TileList> {
    let dest = cfg.tilelist_path();
    if !dest.exists() {
        need(&["curl"])?;
        log!("fetching s3://{}/tileList.txt", cfg.bucket);
        run(cmd(
            "curl",
            &[
                "-fsS",
                "-o",
                dest.to_string_lossy().as_ref(),
                &format!("https://{}.s3.amazonaws.com/tileList.txt", cfg.bucket),
            ],
        ))?;
    }
    let raw = std::fs::read(&dest)?;
    let list = TileList::verify_and_parse(&raw, &TILELIST).inspect_err(|_| {
        // A list that does not match the pin must not be left behind for the
        // next run to read as cached and trusted.
        let _ = std::fs::remove_file(Path::new(&dest));
    })?;
    log!(
        "tileList.txt matches pin: {} tiles, {}",
        list.len(),
        TILELIST.release
    );
    Ok(list)
}
