//! Two PMTiles v3 archives from Copernicus GLO-30: contour vectors and a
//! terrain raster.
//!
//! ONE job, because both artifacts derive from the same 1.5 TB of COGs and the
//! download is the slow part. TWO archives, because PMTiles v3 carries a single
//! `tile_type` byte per file and MVT tiles cannot share a container with PNG.
//!
//! The heavy lifting stays in GDAL's C binaries, tippecanoe and go-pmtiles;
//! this crate is the orchestration, the WebMercatorQuad arithmetic and the
//! Terrain-RGB packing. `gdal_contour … /vsistdout/ | tippecanoe` is built with
//! [`run::Pipeline`], which hands each child's stdout to the next child's stdin
//! as a file descriptor — the same zero-copy stream a shell pipe makes, but
//! with a separate `ExitStatus` per member. That separation is load-bearing:
//! flat ground legitimately has no 1000 m contour and tippecanoe exits non-zero
//! saying so, and telling that apart from a real failure is impossible when the
//! whole pipeline reports one status.

pub mod config;
pub mod contours;
pub mod floor;
pub mod grid;
pub mod logging;
pub mod mbtiles;
pub mod md5;
pub mod pmtiles;
pub mod raster;
pub mod run;
pub mod tiles;
pub mod trgb;

/// Every fallible path in the crate. `?` lifts `io::Error` and `String` alike,
/// which is the whole requirement — nothing matches on an error kind.
pub type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
