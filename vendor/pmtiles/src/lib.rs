// VENDORED. Upstream pulls README.md in as the crate docs whenever default
// features are on. Three of the README's four examples need backends this copy
// deletes (`new_with_path` was mmap, then reqwest, then aws-sdk-s3), so
// including it would be three doctests that cannot compile. The fallback title
// upstream already had for the no-default case is used unconditionally.
#![doc = "# `PMTiles` (for Rust)"]

#[cfg(feature = "__async")]
mod async_reader;
#[cfg(feature = "__async")]
pub use async_reader::{AsyncBackend, AsyncPmTilesReader, BackendResponse};

mod backends;
#[allow(unused_imports, reason = "only a warning if no backends are enabled")]
pub use backends::*;

#[cfg(feature = "__async")]
mod cache;
#[cfg(all(feature = "__async", feature = "moka"))]
pub use cache::MokaCache;
#[cfg(feature = "__async")]
pub use cache::{DirCacheResult, DirectoryCache, HashMapCache, NoCache};

mod directory;
mod error;
mod header;
mod tile;
#[cfg(feature = "write")]
mod writer;

#[cfg(feature = "iter-async")]
pub use directory::DirEntryCoordsIter;
pub use directory::{DirEntry, Directory};
pub use error::{PmtError, PmtResult};
pub use header::{Compression, Header, TileType};
/// Re-export of crate exposed in our API to simplify dependency management
#[cfg(feature = "http-async")]
pub use reqwest;
pub use tile::{MAX_TILE_ID, MAX_ZOOM, PYRAMID_SIZE_BY_ZOOM, TileCoord, TileId};
/// Re-export of crate exposed in our API to simplify dependency management
#[cfg(feature = "tilejson")]
pub use tilejson;
#[cfg(feature = "write")]
pub use writer::{Compressor, PmTilesStreamWriter, PmTilesWriter};

// VENDORED: the pin on the offset width this copy exists to change. Its own
// file because it is the whole reason this directory is here, and because it
// is meant to be run on `i686-unknown-linux-gnu` as well as the host -- see
// the module doc. Native-only for the reason the other test modules are.
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod wide_offset_tests;

#[cfg(test)]
mod tests {
    pub const RASTER_FILE: &str = "fixtures/stamen_toner(raster)CC-BY+ODbL_z3.pmtiles";
    pub const VECTOR_FILE: &str = "fixtures/protomaps(vector)ODbL_firenze.pmtiles";
}
