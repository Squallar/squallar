use super::*;

/// The tile the rest of this crate's fixtures come from.
fn a_tile() -> TileId {
    TileId {
        z: 14,
        x: 8529,
        y: 5974,
    }
}

fn a_frame() -> BoxFrame {
    BoxFrame {
        site: (43.731_414_013_768_99, 7.415_771_484_375),
        x_km: (-3.0, 3.0),
        y_km: (-2.0, 4.0),
    }
}

/// The corners of a tile land where `squallar-geo`'s own tile arithmetic says
/// they do.
///
/// The check is against `tile_to_lon` / `tile_to_lat` rather than against
/// re-derived numbers, because those two are the workspace's definition of
/// where a tile is and a second derivation here would only be able to disagree
/// with them.
#[test]
fn the_corners_of_a_tile_are_where_the_slippy_grid_puts_them() {
    let tile = a_tile();
    let extent = 4096;

    let (north_lat, west_lon) = tile.point_geo(extent, 0.0, 0.0);
    let (south_lat, east_lon) = tile.point_geo(extent, f64::from(extent), f64::from(extent));

    assert!((west_lon - squallar_geo::tile_to_lon(tile.x, tile.z)).abs() < 1e-12);
    assert!((east_lon - squallar_geo::tile_to_lon(tile.x + 1, tile.z)).abs() < 1e-12);
    assert!((north_lat - squallar_geo::tile_to_lat(tile.y, tile.z)).abs() < 1e-12);
    assert!((south_lat - squallar_geo::tile_to_lat(tile.y + 1, tile.z)).abs() < 1e-12);

    // The falsifiability half: the two edges are genuinely apart, so an
    // implementation that answered the same point for both would fail rather
    // than pass four tolerances at once.
    assert!(
        north_lat - south_lat > 1e-3 && east_lon - west_lon > 1e-3,
        "a z14 tile spans {} deg of latitude and {} deg of longitude here, \
         which is too little for the comparisons above to have teeth",
        north_lat - south_lat,
        east_lon - west_lon,
    );
}

/// Extent units run south and everything downstream runs north, so the flip
/// happens **once**.
///
/// Two flips and no flips look identical on a symmetric fixture, which is why
/// this asserts the sign rather than a magnitude.
#[test]
fn the_extent_axis_is_flipped_exactly_once() {
    let tile = a_tile();
    let (top, _) = tile.point_geo(4096, 2048.0, 0.0);
    let (middle, _) = tile.point_geo(4096, 2048.0, 2048.0);
    let (bottom, _) = tile.point_geo(4096, 2048.0, 4096.0);
    assert!(
        top > middle && middle > bottom,
        "latitude runs {top} -> {middle} -> {bottom} down the extent axis; \
         `y` increasing in extent units must mean latitude falling",
    );
}

/// The extent divides, so the same ground point can be spelled at any extent.
#[test]
fn a_point_is_the_same_place_at_any_declared_extent() {
    let tile = a_tile();
    let quarter = tile.point_geo(4096, 1024.0, 3072.0);
    let same = tile.point_geo(8192, 2048.0, 6144.0);
    assert!((quarter.0 - same.0).abs() < 1e-12 && (quarter.1 - same.1).abs() < 1e-12);
    // And a *different* extent-unit point really is a different place, so the
    // equality above is not the function ignoring its arguments.
    let elsewhere = tile.point_geo(4096, 3072.0, 1024.0);
    assert!((quarter.0 - elsewhere.0).abs() > 1e-4 && (quarter.1 - elsewhere.1).abs() > 1e-4);
}

/// [`BoxFrame::geo_to_km`] undoes the forward map the height field's posts are
/// built with, and it undoes it to the metre over a whole box.
///
/// **This is the pin that keeps a building on the ground under it.**
/// `squallar_elevation::resample::post_geo` goes box kilometres ->
/// `great_circle_destination`; this goes the other way. If they were two
/// different projections the buildings would drift from the terrain by
/// hundreds of metres at the box edge, and nothing else in either crate would
/// notice.
#[test]
fn the_projection_undoes_the_forward_map_the_height_posts_are_built_with() {
    let frame = BoxFrame {
        site: (39.0, -106.0),
        x_km: (-460.0, 460.0),
        y_km: (-460.0, 460.0),
    };
    let mut worst: f64 = 0.0;
    for east in [-460.0, -137.0, -0.5, 0.0, 3.25, 200.0, 460.0] {
        for north in [-460.0, -201.0, -1.0, 0.0, 7.5, 311.0, 460.0] {
            // Exactly `post_geo`'s two lines, and not a paraphrase of them.
            let range_km: f64 = f64::hypot(east, north);
            let bearing_deg = f64::atan2(east, north).to_degrees();
            let (lat, lon) = squallar_geo::great_circle_destination(
                frame.site.0,
                frame.site.1,
                bearing_deg,
                range_km,
            );

            let back = frame.geo_to_km(lat, lon);
            worst = worst
                .max((back[0] - east).abs())
                .max((back[1] - north).abs());
        }
    }
    assert!(
        worst < 1e-6,
        "the round trip is out by {worst} km at worst over a 920 km box",
    );

    // The falsifiability half. The same sweep with the bearing's two
    // components swapped -- the single likeliest way to write this wrong --
    // has to be caught by the tolerance above, or the tolerance is not
    // measuring anything.
    let (lat, lon) = squallar_geo::great_circle_destination(
        frame.site.0,
        frame.site.1,
        f64::atan2(200.0, 311.0).to_degrees(),
        f64::hypot(200.0, 311.0),
    );
    let (bearing_deg, range_km) =
        squallar_geo::site_bearing_range_km(frame.site.0, frame.site.1, lat, lon);
    let (sin_b, cos_b) = bearing_deg.to_radians().sin_cos();
    let transposed = [range_km * cos_b, range_km * sin_b];
    assert!(
        (transposed[0] - 200.0).abs() > 1.0,
        "east and north are close enough at this fixture point that swapping \
         them is inside the tolerance; pick a point where they differ",
    );
}

#[test]
fn an_address_off_its_own_grid_is_not_addressable() {
    assert!(TileId { z: 0, x: 0, y: 0 }.is_addressable());
    assert!(TileId { z: 2, x: 3, y: 3 }.is_addressable());
    assert!(!TileId { z: 2, x: 4, y: 0 }.is_addressable());
    assert!(!TileId { z: 2, x: 0, y: 4 }.is_addressable());
    assert!(
        !TileId {
            z: MAX_TILE_ZOOM + 1,
            x: 0,
            y: 0,
        }
        .is_addressable(),
        "a zoom past the ceiling must be refused before the shift that would \
         compute its grid",
    );
    assert!(
        TileId {
            z: MAX_TILE_ZOOM,
            x: (1u32 << MAX_TILE_ZOOM) - 1,
            y: 0,
        }
        .is_addressable(),
        "the deepest zoom's last column is a tile that exists",
    );
}

#[test]
fn a_frame_with_a_non_finite_or_inverted_extent_is_not_drawable() {
    assert!(a_frame().is_drawable());
    for broken in [
        BoxFrame {
            site: (f64::NAN, 7.0),
            ..a_frame()
        },
        BoxFrame {
            x_km: (f64::INFINITY, 3.0),
            ..a_frame()
        },
        BoxFrame {
            x_km: (3.0, -3.0),
            ..a_frame()
        },
        BoxFrame {
            y_km: (1.0, 1.0),
            ..a_frame()
        },
    ] {
        assert!(!broken.is_drawable(), "{broken:?} passed as drawable");
    }
}

/// The cull is the permissive arm: touching counts, and only wholly-outside is
/// dropped.
#[test]
fn overlaps_keeps_anything_that_touches_the_box() {
    let frame = a_frame();
    assert!(
        frame.overlaps([-1.0, 0.0, 1.0, 1.0]),
        "a bbox inside the box"
    );
    assert!(
        frame.overlaps([-3.0, -2.0, -3.0, -2.0]),
        "a bbox exactly on the south-west corner",
    );
    assert!(
        frame.overlaps([-10.0, -10.0, 10.0, 10.0]),
        "a bbox swallowing the box",
    );
    assert!(
        frame.overlaps([2.5, 0.0, 4.0, 1.0]),
        "a bbox straddling the east edge",
    );
    assert!(
        !frame.overlaps([3.0001, 0.0, 4.0, 1.0]),
        "a bbox just east of the box",
    );
    assert!(
        !frame.overlaps([0.0, 4.0001, 1.0, 5.0]),
        "a bbox just north of the box",
    );
}
