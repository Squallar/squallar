//! Project the lat/lon coordinates into a 2D x/y using the Web Mercator.
//! <https://en.wikipedia.org/wiki/Web_Mercator_projection>
//! <https://wiki.openstreetmap.org/wiki/Slippy_map_tilenames>
//! <https://www.netzwolf.info/osm/tilebrowser.html?lat=51.157800&lon=6.865500&zoom=14>

use crate::{
    lon_lat,
    position::{Pixels, Position},
    tiles::TileId,
};
use std::f64::consts::PI;

// zoom level   tile coverage  number of tiles  tile size(*) in degrees
// 0            1 tile         1 tile           360° x 170.1022°
// 1            2 × 2 tiles    4 tiles          180° x 85.0511°
// 2            4 × 4 tiles    16 tiles         90° x [variable]

/// Zoom specifies how many pixels are in the whole map. For example, zoom 0 means that the whole
/// map is just one 256x256 tile, zoom 1 means that it is 2x2 tiles, and so on.
pub(crate) fn total_pixels(zoom: f64) -> f64 {
    2f64.powf(zoom) * (TILE_SIZE as f64)
}

/// Number of tiles along one axis of the grid at `zoom`.
///
/// `None` above zoom 31: the grid is counted in `u32` but [`TileId::zoom`] is a
/// `u8`, so a tile id can name a zoom that has no answer here.
pub fn total_tiles(zoom: u8) -> Option<u32> {
    2u32.checked_pow(zoom as u32)
}

/// Size of a single tile in pixels. Walkers uses 256px tiles as most of the tile sources do.
const TILE_SIZE: u32 = 256;

/// Project the position into the Mercator projection and normalize it to 0-1 range.
fn mercator_normalized(position: Position) -> (f64, f64) {
    // Project into Mercator (cylindrical map projection).
    let x = position.x().to_radians();
    let y = position.y().to_radians().tan().asinh();

    // Scale both x and y to 0-1 range.
    let x = (1. + (x / PI)) / 2.;
    let y = (1. - (y / PI)) / 2.;

    (x, y)
}

/// Calculate the tile coordinated for the given position.
pub(crate) fn tile_id(position: Position, mut zoom: u8, source_tile_size: u32) -> TileId {
    let (x, y) = mercator_normalized(position);

    // Some sources provide larger tiles, effectively bundling e.g. 4 256px tiles in one
    // 512px one. Walkers uses 256px internally, so we need to adjust the zoom level.
    //
    // Saturating, because there is no shallower level to reduce to at the top of the grid
    // and a plain `-=` is a debug-build panic there: a 512px source at map zoom 0 asked for
    // `0 - 1`. Zoom 0 is the right answer anyway — the source's own zoom-0 tile is the whole
    // world, which is what the one tile of the zoom-0 grid is, just drawn at half the
    // texture's size.
    zoom = zoom.saturating_sub((source_tile_size as f64 / TILE_SIZE as f64).log2() as u8);

    // Map that into a big bitmap made out of web tiles.
    let number_of_tiles = 2u32.pow(zoom as u32) as f64;
    let x = (x * number_of_tiles).floor() as u32;
    let y = (y * number_of_tiles).floor() as u32;

    TileId { x, y, zoom }
}

/// Project geographical position into a 2D plane using Mercator.
pub fn project(position: Position, zoom: f64) -> Pixels {
    project_at_scale(position, total_pixels(zoom))
}

/// [`project`], for a caller that already holds [`total_pixels`] for its zoom.
///
/// `2f64.powf(zoom)` is not free and zoom is fixed for a whole frame, so a caller
/// projecting many points pays it once instead of once per point.
pub(crate) fn project_at_scale(position: Position, total_pixels: f64) -> Pixels {
    let (x, y) = mercator_normalized(position);
    Pixels::new(x * total_pixels, y * total_pixels)
}

/// Transforms screen pixels into a geographical position.
pub(crate) fn unproject(pixels: Pixels, zoom: f64) -> Position {
    unproject_at_scale(pixels, total_pixels(zoom))
}

/// [`unproject`], for a caller that already holds [`total_pixels`] for its zoom.
pub(crate) fn unproject_at_scale(pixels: Pixels, total_pixels: f64) -> Position {
    let lon = pixels.x();
    let lon = lon / total_pixels;
    let lon = (lon * 2. - 1.) * PI;
    let lon = lon.to_degrees();

    let lat = pixels.y();
    let lat = lat / total_pixels;
    let lat = (-lat * 2. + 1.) * PI;
    let lat = lat.sinh().atan().to_degrees();

    lon_lat(lon, lat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lat_lon;

    #[test]
    fn projecting_position_and_tile() {
        let citadel = lon_lat(21.00027, 52.26470);

        // Just a bit higher than what most providers support,
        // to make sure we cover the worst case in terms of precision.
        let zoom = 20;

        assert_eq!(
            TileId {
                x: 585455,
                y: 345104,
                zoom
            },
            tile_id(citadel, zoom, 256)
        );

        // Automatically zooms out for larger tiles
        assert_eq!(
            TileId {
                x: 292727,
                y: 172552,
                zoom: zoom - 1
            },
            tile_id(citadel, zoom, 512)
        );

        // Projected tile is just its x, y multiplied by the size of tiles.
        assert_eq!(
            Pixels::new(585455. * 256., 345104. * 256.),
            tile_id(citadel, zoom, 256).project(256.)
        );

        // Projected Citadel position should be somewhere near projected tile, shifted only by the
        // position on the tile.
        let calculated = project(citadel, zoom as f64);
        let citadel_proj = Pixels::new(585455. * 256. + 184., 345104. * 256. + 116.5);
        approx::assert_relative_eq!(calculated.x(), citadel_proj.x(), max_relative = 0.5);
        approx::assert_relative_eq!(calculated.y(), citadel_proj.y(), max_relative = 0.5);
    }

    /// A source with tiles larger than 256 px is served a shallower zoom, and at
    /// the top of the grid there is no shallower zoom to serve. The reduction
    /// used to be a plain `-=` on a `u8`, so this was a debug-build panic —
    /// reachable from `draw_tiles` with any 512 px source and the map zoomed all
    /// the way out.
    #[test]
    fn a_large_source_tile_at_the_top_of_the_grid_does_not_underflow() {
        let citadel = lon_lat(21.00027, 52.26470);

        for source_tile_size in [512u32, 1024] {
            assert_eq!(
                TileId {
                    x: 0,
                    y: 0,
                    zoom: 0
                },
                tile_id(citadel, 0, source_tile_size)
            );
        }

        // The control: away from the top of the grid the reduction still bites,
        // so the saturation is not simply swallowing it everywhere.
        assert_eq!(3, tile_id(citadel, 4, 512).zoom);
        assert_eq!(2, tile_id(citadel, 4, 1024).zoom);
    }

    /// Zoom is a `u8` but the tile grid is counted in `u32`, so zoom 32 and above
    /// have no answer. `2u32.pow` panics there rather than saying so.
    #[test]
    fn total_tiles_has_no_answer_past_the_u32_grid() {
        for zoom in 0..=31u8 {
            assert_eq!(Some(2u32.pow(zoom as u32)), total_tiles(zoom));
        }

        for zoom in 32..=255u8 {
            assert_eq!(None, total_tiles(zoom));
        }
    }

    /// A spread wide enough that a changed operation order shows up somewhere.
    fn probes() -> impl Iterator<Item = (Position, f64)> {
        const LATS: [f64; 7] = [-85.0, -33.87, 0.0, 0.5, 51.09916, 71.0, 85.0];
        const LONS: [f64; 7] = [-179.9, -122.4194, -0.1276, 0.0, 17.03664, 151.2093, 179.9];

        (0..=38u32).flat_map(|half_zoom| {
            let zoom = half_zoom as f64 * 0.5;
            LATS.iter()
                .flat_map(move |&lat| LONS.iter().map(move |&lon| (lon_lat(lon, lat), zoom)))
        })
    }

    /// Handing [`project_at_scale`] a precomputed [`total_pixels`] must produce the
    /// very same `f64`s as recomputing the scale from the zoom did, not merely a
    /// close one — the operations and their order are meant to be unchanged, so
    /// this is `assert_eq!` on the bits and not an approximate comparison.
    #[test]
    fn a_precomputed_scale_projects_to_the_same_bits() {
        for (position, zoom) in probes() {
            // The expression `project` carried before the scale was hoisted out.
            let scale = 2f64.powf(zoom) * (TILE_SIZE as f64);
            let (x, y) = mercator_normalized(position);
            let expected = Pixels::new(x * scale, y * scale);

            let actual = project_at_scale(position, total_pixels(zoom));

            assert_eq!(expected.x().to_bits(), actual.x().to_bits(), "{position:?}");
            assert_eq!(expected.y().to_bits(), actual.y().to_bits(), "{position:?}");
            assert_eq!(
                expected.x().to_bits(),
                project(position, zoom).x().to_bits()
            );
            assert_eq!(
                expected.y().to_bits(),
                project(position, zoom).y().to_bits()
            );
        }
    }

    /// The same, for [`unproject_at_scale`], which had its own copy of the
    /// `2f64.powf(zoom) * TILE_SIZE` expression rather than calling
    /// [`total_pixels`].
    #[test]
    fn a_precomputed_scale_unprojects_to_the_same_bits() {
        for (position, zoom) in probes() {
            let pixels = project(position, zoom);

            // The expression `unproject` carried before the scale was hoisted out.
            let number_of_pixels: f64 = 2f64.powf(zoom) * (TILE_SIZE as f64);
            let lon = pixels.x();
            let lon = lon / number_of_pixels;
            let lon = (lon * 2. - 1.) * PI;
            let lon = lon.to_degrees();
            let lat = pixels.y();
            let lat = lat / number_of_pixels;
            let lat = (-lat * 2. + 1.) * PI;
            let lat = lat.sinh().atan().to_degrees();
            let expected = lon_lat(lon, lat);

            let actual = unproject_at_scale(pixels, total_pixels(zoom));

            assert_eq!(expected.x().to_bits(), actual.x().to_bits(), "{position:?}");
            assert_eq!(expected.y().to_bits(), actual.y().to_bits(), "{position:?}");
            assert_eq!(
                expected.x().to_bits(),
                unproject(pixels, zoom).x().to_bits()
            );
            assert_eq!(
                expected.y().to_bits(),
                unproject(pixels, zoom).y().to_bits()
            );
        }
    }

    #[test]
    fn project_there_and_back() {
        let citadel = lat_lon(21.00027, 52.26470);
        let zoom = 16;
        let calculated = unproject(project(citadel, zoom as f64), zoom as f64);

        approx::assert_relative_eq!(calculated.x(), citadel.x(), max_relative = 1.0);
        approx::assert_relative_eq!(calculated.y(), citadel.y(), max_relative = 1.0);
    }
}
