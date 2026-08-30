//! End-to-end checks that need GDAL and a real Copernicus tile.
//!
//! `#[ignore]` rather than a silent skip: a test that quietly passes when its
//! prerequisites are missing is a gate that cannot fail. Run them explicitly:
//!
//! ```text
//! cargo test -- --ignored
//! TERRAIN_PROBE=/path/to/N39_00_W106_00.tif cargo test -- --ignored
//! ```
//!
//! The default probe path is where this build's fixture lives; every test here
//! PANICS with a clear message if GDAL or the probe is absent.

use std::path::{Path, PathBuf};
use std::process::Command;

use squallar_terrain::grid::{self, TileRange};
use squallar_terrain::{raster, trgb};

const DEFAULT_PROBE: &str = "/home/reddragon/basemap-build/contour-probe/N39_00_W106_00.tif";

fn probe() -> PathBuf {
    let p =
        PathBuf::from(std::env::var("TERRAIN_PROBE").unwrap_or_else(|_| DEFAULT_PROBE.to_string()));
    assert!(
        p.exists(),
        "probe tile {} is missing; set TERRAIN_PROBE",
        p.display()
    );
    p
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("squallar-terrain-pipeline-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn gdal(program: &str, args: &[&str]) {
    let out = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("{program} is not on PATH: {e}"));
    assert!(
        out.status.success(),
        "{program} {args:?} exited {:?}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Warp the probe onto one exact z12 tile and return the raw Float32 plane.
fn warp_one_tile(dir: &Path, range: TileRange) -> (grid::Extent, PathBuf) {
    let e = grid::tile_extent(12, range);
    let elev = dir.join("elev.img");
    gdal(
        "gdalwarp",
        &[
            "-q",
            "-overwrite",
            "-of",
            "ENVI",
            "-t_srs",
            "EPSG:3857",
            "-te",
            &format!("{:.10}", e.xmin),
            &format!("{:.10}", e.ymin),
            &format!("{:.10}", e.xmax),
            &format!("{:.10}", e.ymax),
            "-ts",
            &e.nx.to_string(),
            &e.ny.to_string(),
            "-r",
            "average",
            "-ot",
            "Float32",
            "-dstnodata",
            "0",
            probe().to_string_lossy().as_ref(),
            elev.to_string_lossy().as_ref(),
        ],
    );
    (e, elev)
}

/// The tile range covering the probe's centre at z12.
fn probe_tile() -> TileRange {
    let r = grid::tile_range(12, -105.55, 39.45, -105.55, 39.45);
    assert_eq!((r.tx0, r.tx1), (r.tx0, r.tx0), "expected a single tile");
    r
}

/// `gdalwarp -of ENVI` writes exactly `nx*ny*4` bytes with no header.
///
/// This is the whole contract the packer reads against. Every GDAL raw driver
/// is Create()-based and needs a seekable output, so this cannot be a pipe —
/// `-of ENVI /vsistdout/` fails with "ERROR 6: Read or update mode not
/// supported on /vsistdout", and a FIFO hangs on the reopen.
#[test]
#[ignore]
fn a_warped_envi_plane_is_exactly_the_pixel_count() {
    let dir = scratch("envi");
    let (e, elev) = warp_one_tile(&dir, probe_tile());
    assert_eq!((e.nx, e.ny), (256, 256));
    let got = std::fs::metadata(&elev).unwrap().len();
    assert_eq!(got, u64::from(e.nx) * u64::from(e.ny) * 4);

    let hdr = std::fs::read_to_string(elev.with_extension("hdr")).unwrap();
    for want in ["header offset = 0", "data type = 4", "byte order = 0"] {
        assert!(hdr.contains(want), "ENVI header lacks {want:?}:\n{hdr}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// GDAL reads the raw VRT's interleave the way the packer wrote it.
///
/// The band offsets are the part a typo breaks silently: a wrong `ImageOffset`
/// still produces a plausible image, just with the channels rotated, and one
/// count of error in R is 6553.6 m. Reading each band back separately is what
/// proves the wiring rather than assuming it.
#[test]
#[ignore]
fn the_raw_vrt_hands_gdal_the_bands_in_the_written_order() {
    let dir = scratch("vrt");
    let (e, elev) = warp_one_tile(&dir, probe_tile());
    let vrt = raster::pack_terrain_rgb(&e, &elev).unwrap();
    let packed = std::fs::read(elev.with_extension("rgb.bin")).unwrap();
    let pixels = (e.nx as usize) * (e.ny as usize);
    assert_eq!(packed.len(), pixels * 3);

    for band in 1..=3usize {
        let out = dir.join(format!("b{band}.img"));
        gdal(
            "gdal_translate",
            &[
                "-q",
                "-of",
                "ENVI",
                "-b",
                &band.to_string(),
                vrt.to_string_lossy().as_ref(),
                out.to_string_lossy().as_ref(),
            ],
        );
        let read_back = std::fs::read(&out).unwrap();
        assert_eq!(read_back.len(), pixels, "band {band} pixel count");
        let mine: Vec<u8> = (0..pixels).map(|i| packed[i * 3 + band - 1]).collect();
        assert_eq!(
            read_back, mine,
            "band {band} does not match what was packed"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// The elevation survives the whole chain: warp, pack, VRT, GDAL, unpack.
///
/// Half a quantum is 0.05 m; anything larger means a carry was dropped.
#[test]
#[ignore]
fn elevation_round_trips_through_the_packed_archive() {
    let dir = scratch("roundtrip");
    let (e, elev) = warp_one_tile(&dir, probe_tile());
    let vrt = raster::pack_terrain_rgb(&e, &elev).unwrap();

    let rgb = dir.join("rgb_read.img");
    gdal(
        "gdal_translate",
        &[
            "-q",
            "-of",
            "ENVI",
            "-co",
            "INTERLEAVE=BIP",
            vrt.to_string_lossy().as_ref(),
            rgb.to_string_lossy().as_ref(),
        ],
    );

    let heights = std::fs::read(&elev).unwrap();
    let decoded = std::fs::read(&rgb).unwrap();
    let pixels = (e.nx as usize) * (e.ny as usize);
    assert_eq!(decoded.len(), pixels * 3);

    let (mut worst, mut spread) = (0.0f64, 0.0f64);
    for i in 0..pixels {
        let h = f64::from(f32::from_le_bytes([
            heights[i * 4],
            heights[i * 4 + 1],
            heights[i * 4 + 2],
            heights[i * 4 + 3],
        ]));
        let back = trgb::unpack([decoded[i * 3], decoded[i * 3 + 1], decoded[i * 3 + 2]]);
        worst = worst.max((back - h).abs());
        spread = spread.max(h.abs());
    }
    assert!(worst <= 0.05 + 1e-9, "worst round-trip error {worst} m");
    // Non-triviality: an all-zero tile would round-trip perfectly and prove
    // nothing, so the tile has to carry real relief.
    assert!(
        spread > 1000.0,
        "tile is flat ({spread} m); this proves nothing"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// The MBTiles driver infers the zoom from the resolution, and the grid this
/// build warps onto is what makes that inference land on the right one.
#[test]
#[ignore]
fn the_packed_vrt_tiles_at_the_zoom_it_was_warped_to() {
    let dir = scratch("mbtiles");
    let (e, elev) = warp_one_tile(&dir, probe_tile());
    let vrt = raster::pack_terrain_rgb(&e, &elev).unwrap();
    let mb = dir.join("out.mbtiles");
    gdal(
        "gdal_translate",
        &[
            "-q",
            "-of",
            "MBTILES",
            "-co",
            "TILE_FORMAT=PNG",
            vrt.to_string_lossy().as_ref(),
            mb.to_string_lossy().as_ref(),
        ],
    );
    let zooms = squallar_terrain::mbtiles::zoom_levels(&mb).unwrap();
    assert_eq!(zooms, vec![12], "warped to z12 but archive holds {zooms:?}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A flat tile has no 1000 m contour, tippecanoe exits non-zero saying so, and
/// that is data rather than failure — but only because every producer in the
/// pipeline is known to have succeeded first.
#[test]
#[ignore]
fn a_contour_interval_with_no_features_is_distinguishable_from_a_failure() {
    use squallar_terrain::run::{Pipeline, cmd};

    let dir = scratch("contour");
    let out = dir.join("empty.pmtiles");

    // 100000 m contours over ground that tops out at 4.3 km: no features.
    let mut pipe = Pipeline::to(cmd(
        "tippecanoe",
        &[
            "-q",
            "--force",
            "-Z10",
            "-z10",
            "-l",
            "contour",
            "-y",
            "elev",
            "--no-feature-limit",
            "--no-tile-size-limit",
            "-t",
            dir.to_string_lossy().as_ref(),
            "-o",
            out.to_string_lossy().as_ref(),
        ],
    ))
    .unwrap();
    pipe.feed(cmd(
        "gdal_contour",
        &[
            "-q",
            "-a",
            "elev",
            "-i",
            "100000",
            "-f",
            "GeoJSONSeq",
            probe().to_string_lossy().as_ref(),
            "/vsistdout/",
        ],
    ))
    .unwrap();
    let r = pipe.finish().unwrap();
    assert!(
        !r.status.success(),
        "expected tippecanoe to refuse an empty stream"
    );
    assert!(
        r.stderr.contains("Did not read any valid geometries"),
        "tippecanoe said something else:\n{}",
        r.stderr
    );

    // The control: a producer that FAILS must be reported as a producer
    // failure, not mistaken for the flat-ground case above.
    let mut pipe = Pipeline::to(cmd(
        "tippecanoe",
        &[
            "-q",
            "--force",
            "-Z10",
            "-z10",
            "-l",
            "contour",
            "-y",
            "elev",
            "-t",
            dir.to_string_lossy().as_ref(),
            "-o",
            dir.join("bad.pmtiles").to_string_lossy().as_ref(),
        ],
    ))
    .unwrap();
    pipe.feed(cmd(
        "gdal_contour",
        &[
            "-q",
            "-a",
            "elev",
            "-i",
            "100",
            "-f",
            "GeoJSONSeq",
            "/nonexistent/tile.tif",
            "/vsistdout/",
        ],
    ))
    .unwrap();
    let err = pipe.finish().unwrap_err().to_string();
    assert!(
        err.contains("gdal_contour"),
        "producer failure was not named: {err}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// `floor::min_of_cog` against the real `gdalinfo` binary, on a raster this
/// test writes itself.
///
/// ENVI rather than GeoTIFF: the format is a raw plane plus a text header, so
/// the fixture needs no GDAL to CREATE and the test exercises the read and the
/// parse alone. `#[ignore]` for the same reason as everything else in this file
/// — it needs GDAL on PATH, and a test that quietly passes without its
/// prerequisite is a gate that cannot fail.
#[test]
#[ignore]
fn a_synthetic_raster_is_read_through_real_gdalinfo() {
    let dir = scratch("floor-min");
    let img = dir.join("synthetic.img");
    // Death Valley's published minimum among fifteen ordinary heights, and last
    // in the plane, so the answer cannot be the first pixel or the mean.
    let values: [f32; 16] = [
        100.0, 250.5, 4053.7, 0.0, -12.25, 900.0, 1200.0, 3000.0, 2396.2, -3.5, 55.0, 7.0, 8.0,
        9.0, 10.0, -86.4,
    ];
    let mut raw = Vec::new();
    for v in values {
        raw.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(&img, raw).unwrap();
    std::fs::write(
        dir.join("synthetic.hdr"),
        "ENVI\nsamples = 4\nlines = 4\nbands = 1\nheader offset = 0\n\
         file type = ENVI Standard\ndata type = 4\ninterleave = bsq\nbyte order = 0\n\
         map info = {Geographic Lat/Lon, 1, 1, -117.0, 37.0, 0.25, 0.25, WGS-84}\n",
    )
    .unwrap();

    let got = squallar_terrain::floor::min_of_cog(img.to_string_lossy().as_ref())
        .expect("gdalinfo -mm reads the plane");
    assert!(
        (got + 86.4).abs() < 1e-3,
        "gdalinfo reported {got} m, not the -86.4 m written into the plane"
    );
    let _ = std::fs::remove_dir_all(dir);
}
