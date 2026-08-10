use super::*;

/// A synthetic source: side 64, transparent everywhere except an opaque
/// red texel at a chosen pixel.
fn source_with_dot(side: usize, at: (usize, usize)) -> Vec<u8> {
    let mut image = vec![0u8; side * side * 4];
    let idx = (at.1 * side + at.0) * 4;
    image[idx..idx + 4].copy_from_slice(&[255, 0, 0, 255]);
    image
}

/// Where the floor put a colour above the ground, as (col, row) of the
/// brightest red texel.
fn brightest_red(floor: &FloorImage) -> (usize, usize) {
    let mut best = (0, 0);
    let mut best_red = 0u8;
    for row in 0..floor.size[1] as usize {
        for col in 0..floor.size[0] as usize {
            let at = (row * floor.size[0] as usize + col) * 4;
            if floor.rgba[at] > best_red {
                best_red = floor.rgba[at];
                best = (col, row);
            }
        }
    }
    assert!(
        best_red > FLOOR_GROUND_RGBA[0],
        "no echo landed on the floor"
    );
    best
}

/// The site's own pixel lands in the middle of a site-centred floor.
///
/// The centre is the one point every projection convention agrees on, so
/// this is the control; the offset cases below are the test.
#[test]
fn the_sites_pixel_lands_in_the_middle_of_a_site_centred_floor() {
    let side = 64;
    let source = source_with_dot(side, (32, 32));
    let floor = resample_floor(&source, 35.0, (-230.0, 230.0), (-230.0, 230.0))
        .expect("a resamplable floor");
    let (col, row) = brightest_red(&floor);
    let mid = FLOOR_TEXELS as usize / 2;
    assert!(
        col.abs_diff(mid) <= 8 && row.abs_diff(mid) <= 8,
        "the site's echo landed at ({col}, {row}) of {FLOOR_TEXELS}, not the centre",
    );
}

/// A dot north-east of the site lands in the floor's upper-right quadrant,
/// and the vertical placement follows the raster's Mercator spacing.
///
/// Two mutations this closes, both of which leave the centred case green:
/// flipping the v axis (north landing at the bottom), and reading the
/// footprint through a linear-latitude mapping instead of the raster's
/// Mercator one — at 35° N the Mercator rows are measurably denser towards
/// the equator, so the linear read puts the dot rows away from where the
/// raster drew it.
#[test]
fn an_echo_north_east_of_the_site_lands_north_east_and_on_the_mercator_row() {
    let side = 256;
    // The raster pixel where its own forward projection puts a point
    // 150 km north, 100 km east of a site at 35 N: run the forward
    // arithmetic from render_gate.
    let site_lat_rad = 35.0f64.to_radians();
    let lat_rad = site_lat_rad + 150.0 / rustdar_radar::types::EARTH_RADIUS_KM;
    let px_per_km = side as f64 / 460.0;
    let px = side as f64 / 2.0 + 100.0 * (site_lat_rad.cos() / lat_rad.cos()) * px_per_km;
    let max_lat: f64 = 35.0 + 230.0 / 111.32;
    let min_lat: f64 = 35.0 - 230.0 / 111.32;
    let merc_top = mercator_y(max_lat.to_radians());
    let merc_scale = side as f64 / (merc_top - mercator_y(min_lat.to_radians()));
    let py = (merc_top - mercator_y(lat_rad)) * merc_scale;

    let source = source_with_dot(side, (px as usize, py as usize));
    let floor = resample_floor(&source, 35.0, (-230.0, 230.0), (-230.0, 230.0))
        .expect("a resamplable floor");
    let (col, row) = brightest_red(&floor);

    // Expected floor texel: the footprint is linear in km, so 150 km north
    // of a ±230 km box is (230 - 150) / 460 of the way down from the top.
    let want_col = ((100.0 + 230.0) / 460.0 * FLOOR_TEXELS as f64) as usize;
    let want_row = ((230.0 - 150.0) / 460.0 * FLOOR_TEXELS as f64) as usize;
    assert!(
        col.abs_diff(want_col) <= 4,
        "east placement: got column {col}, the box arithmetic says {want_col}",
    );
    assert!(
        row.abs_diff(want_row) <= 4,
        "north placement: got row {row}, the box arithmetic says {want_row} — \
             a v flip or a linear-latitude read both move it here",
    );
}

/// A region footprint off the site's centre reads the matching part of
/// the raster: the same dot, through a box that puts it at the box centre.
#[test]
fn an_off_centre_footprint_reads_the_matching_part_of_the_raster() {
    let side = 256;
    let site_lat_rad = 35.0f64.to_radians();
    let lat_rad = site_lat_rad + 150.0 / rustdar_radar::types::EARTH_RADIUS_KM;
    let px_per_km = side as f64 / 460.0;
    let px = side as f64 / 2.0 + 100.0 * (site_lat_rad.cos() / lat_rad.cos()) * px_per_km;
    let max_lat: f64 = 35.0 + 230.0 / 111.32;
    let min_lat: f64 = 35.0 - 230.0 / 111.32;
    let merc_top = mercator_y(max_lat.to_radians());
    let merc_scale = side as f64 / (merc_top - mercator_y(min_lat.to_radians()));
    let py = (merc_top - mercator_y(lat_rad)) * merc_scale;

    let source = source_with_dot(side, (px as usize, py as usize));
    // A 80 km-wide box centred on the dot's (100, 150) km offset.
    let floor =
        resample_floor(&source, 35.0, (60.0, 140.0), (110.0, 190.0)).expect("a resamplable floor");
    let (col, row) = brightest_red(&floor);
    let mid = FLOOR_TEXELS as usize / 2;
    assert!(
        col.abs_diff(mid) <= 8 && row.abs_diff(mid) <= 8,
        "a box centred on the echo must put it at the floor's centre, got \
             ({col}, {row})",
    );
}

/// Where no echo painted, the floor is the opaque ground colour — never
/// transparent, never the raster's transparent black leaking through.
#[test]
fn bare_ground_is_the_ground_colour_and_opaque() {
    let source = vec![0u8; 64 * 64 * 4];
    let floor = resample_floor(&source, 35.0, (-230.0, 230.0), (-230.0, 230.0))
        .expect("a resamplable floor");
    assert_eq!(&floor.rgba[..4], &FLOOR_GROUND_RGBA);
    assert!(
        floor.rgba.chunks_exact(4).all(|px| px[3] == 255),
        "the floor must be opaque ground everywhere",
    );
}

/// Plant the same geographic point `(dx_km, dy_km)` east/north of a
/// 35 N site as a radar-raster dot AND a tile-pixel dot — two
/// independent forward routes — compose, and return where each landed
/// on the floor, as `(radar texel, tile texel)`.
fn tile_and_gate_texels(dx_km: f64, dy_km: f64) -> ((usize, usize), (usize, usize)) {
    let side = 256;
    let (site_lat, site_lon) = (35.0f64, -97.0f64);

    // The radar raster's dot, through the raster's forward projection.
    let site_lat_rad = site_lat.to_radians();
    let lat_rad = site_lat_rad + dy_km / rustdar_radar::types::EARTH_RADIUS_KM;
    let px_per_km = side as f64 / 460.0;
    let px = side as f64 / 2.0 + dx_km * (site_lat_rad.cos() / lat_rad.cos()) * px_per_km;
    let max_lat: f64 = site_lat + 230.0 / 111.32;
    let min_lat: f64 = site_lat - 230.0 / 111.32;
    let merc_top = mercator_y(max_lat.to_radians());
    let merc_scale = side as f64 / (merc_top - mercator_y(min_lat.to_radians()));
    let py = (merc_top - mercator_y(lat_rad)) * merc_scale;
    let source = source_with_dot(side, (px as usize, py as usize));

    // The tile's dot, through the slippy pyramid's forward formulas.
    let zoom = floor_tile_zoom(site_lat, (-230.0, 230.0), 18);
    let n = f64::from(1u32 << zoom);
    let lon = site_lon + dx_km / (111.32 * lat_rad.cos());
    let tile_x_f = (lon + 180.0) / 360.0 * n;
    let tile_y_f = (1.0 - mercator_y(lat_rad) / PI) / 2.0 * n;
    let (tile_x, tile_y) = (tile_x_f.floor(), tile_y_f.floor());
    let tile_side = 256usize;
    let mut tile_rgba = vec![0u8; tile_side * tile_side * 4];
    let (dot_px, dot_py) = (
        ((tile_x_f - tile_x) * tile_side as f64) as usize,
        ((tile_y_f - tile_y) * tile_side as f64) as usize,
    );
    for row in dot_py.saturating_sub(1)..=(dot_py + 1).min(tile_side - 1) {
        for col in dot_px.saturating_sub(1)..=(dot_px + 1).min(tile_side - 1) {
            let at = (row * tile_side + col) * 4;
            tile_rgba[at..at + 4].copy_from_slice(&[0, 255, 0, 255]);
        }
    }
    let base = TileLayer {
        zoom,
        tiles: vec![DecodedTile {
            x: tile_x as u32,
            y: tile_y as u32,
            side: tile_side as u32,
            rgba: tile_rgba,
        }],
    };

    let floor = compose_floor(
        &source,
        site_lat,
        site_lon,
        (-230.0, 230.0),
        (-230.0, 230.0),
        &base,
        &TileLayer::empty(),
        &FloorVectors::none(),
    )
    .expect("a composable floor");

    let (red_col, red_row) = brightest_red(&floor);
    let mut best_green = (0usize, 0usize, 0u8);
    for row in 0..floor.size[1] as usize {
        for col in 0..floor.size[0] as usize {
            let at = (row * floor.size[0] as usize + col) * 4;
            let greenness = floor.rgba[at + 1].saturating_sub(floor.rgba[at]);
            if greenness > best_green.2 {
                best_green = (col, row, greenness);
            }
        }
    }
    assert!(best_green.2 > 100, "the tile dot never reached the floor");
    ((red_col, red_row), (best_green.0, best_green.1))
}

/// The floor texels a planted **warning-polygon vertex** at the same
/// `(dx_km, dy_km)` painted, through the third consumer: a tiny blue
/// triangle whose first vertex is the probe point, its geo coordinates
/// produced by the same forward lines the tile route uses, drawn as an
/// over-radar shape into an otherwise empty floor.
fn planted_shape_cluster(dx_km: f64, dy_km: f64) -> Vec<(usize, usize)> {
    let (site_lat, site_lon) = (35.0f64, -97.0f64);
    let site_lat_rad = site_lat.to_radians();
    let lat_rad = site_lat_rad + dy_km / rustdar_radar::types::EARTH_RADIUS_KM;
    let lon = site_lon + dx_km / (111.32 * lat_rad.cos());
    let lat = lat_rad.to_degrees();
    // Two more vertices ~0.45 km (half a texel) north and east: the
    // stroke must stay a cluster *at* the probe, because the pin
    // measures the cluster's nearest texel and every texel of slack here
    // is a texel of projection error the pin can no longer see.
    let dlat = (0.45f64 / rustdar_radar::types::EARTH_RADIUS_KM).to_degrees();
    let dlon = 0.45 / (111.32 * lat_rad.cos());
    let shape = FloorShape {
        ring: vec![(lat, lon), (lat + dlat, lon), (lat, lon + dlon)],
        fill_rgba: [0, 0, 0, 0],
        stroke_rgba: [0, 0, 255, 255],
    };
    let floor = compose_floor(
        &vec![0u8; 64 * 64 * 4],
        site_lat,
        site_lon,
        (-230.0, 230.0),
        (-230.0, 230.0),
        &TileLayer::empty(),
        &TileLayer::empty(),
        &FloorVectors {
            under_radar: Vec::new(),
            over_radar: vec![shape],
            range_ring: false,
        },
    )
    .expect("a composable floor");
    let mut cluster = Vec::new();
    for row in 0..floor.size[1] as usize {
        for col in 0..floor.size[0] as usize {
            let at = (row * floor.size[0] as usize + col) * 4;
            let blueness =
                floor.rgba[at + 2].saturating_sub(floor.rgba[at].max(floor.rgba[at + 1]));
            if blueness > 100 {
                cluster.push((col, row));
            }
        }
    }
    cluster
}

/// One mapping, **three** consumers: a warning-polygon vertex naming the
/// same ground as the radar gate and the tile pixel lands on the same
/// floor texel — probed mid-box, at the box's far corner, and on the
/// site's own parallel, because the two classic wrong-inverses die at
/// *different* probes and each leaves the others green:
///
/// * the site's `cos φ₀` where the vertex's `cos φ` belongs drifts with
///   distance from the site's parallel — under 2 texels at (100, 150),
///   5 at (−200, −190), and exactly **zero** at `dy = 0` — so only the
///   corner kills it (measured: 5 texels there, this test's message);
/// * a Mercator-row read of the vertex — treating the floor's rows like
///   the raster image's — agrees at the box's row *edges* by
///   construction and misses most in the middle: ~1.9 texels at
///   `dy = 150`, ~1 at the corner, 3.2 on the site's parallel — so only
///   the `(120, 0)` probe kills it (measured: 3 texels there).
///
/// The oracle is the *other two consumers*, not a restated formula: the
/// shape cluster must sit within 2 texels of both the gate's texel and
/// the tile's, and stay a compact cluster (a smeared or duplicated
/// stroke is its own failure).
#[test]
fn a_warning_vertex_lands_on_the_texel_the_gate_and_the_tile_name() {
    for (dx_km, dy_km) in [(100.0, 150.0), (-200.0, -190.0), (120.0, 0.0)] {
        let (gate, tile) = tile_and_gate_texels(dx_km, dy_km);
        let cluster = planted_shape_cluster(dx_km, dy_km);
        assert!(
            !cluster.is_empty(),
            "at ({dx_km}, {dy_km}) km the warning vertex never reached the floor",
        );
        assert!(
            cluster.len() < 40,
            "at ({dx_km}, {dy_km}) km the shape smeared into {} texels",
            cluster.len(),
        );
        for (name, (col, row)) in [("gate", gate), ("tile", tile)] {
            let nearest = cluster
                .iter()
                .map(|(c, r)| c.abs_diff(col).max(r.abs_diff(row)))
                .min()
                .unwrap();
            assert!(
                nearest <= 2,
                "at ({dx_km}, {dy_km}) km the warning vertex landed {nearest} \
                     texels from the {name}'s texel — the third consumer of the \
                     mapping has parted from it",
            );
        }
    }
}

/// One mapping, two consumers: a tile pixel and a radar gate that name
/// the same ground land on the same floor texel — probed mid-box AND at
/// the box's far corner.
///
/// The radar dot is planted through `render_gate`'s forward arithmetic
/// (as the Mercator-row test above does) and the tile dot through the
/// slippy pyramid's own forward formulas — two independent routes to the
/// same geographic point. If the composite ever grew a second projection
/// for tiles, this is where the two dots would part.
///
/// The corner probe is not decoration: the mapping's `cos φ` is the
/// *row's* latitude, and the plausible second-projection error — reading
/// the site's `cos φ₀` instead — grows with distance from the site's
/// parallel. At (100, 150) km it drifts under 2 texels and the mid-box
/// probe alone would let it live; at (−200, −190) km it reaches ~5
/// texels and dies here.
#[test]
fn a_tile_pixel_and_a_radar_gate_at_the_same_ground_land_on_the_same_texel() {
    for (dx_km, dy_km) in [(100.0, 150.0), (-200.0, -190.0)] {
        let ((red_col, red_row), (green_col, green_row)) = tile_and_gate_texels(dx_km, dy_km);
        let apart = green_col.abs_diff(red_col).max(green_row.abs_diff(red_row));
        assert!(
            apart <= 2,
            "at ({dx_km}, {dy_km}) km the radar gate and the tile pixel \
                 for the same ground landed {apart} texels apart — the two \
                 consumers of the mapping have parted",
        );
    }
}

/// The stacking order is the 2D pane's: ground, basemap, radar, labels.
#[test]
fn the_layers_stack_ground_basemap_radar_labels() {
    // A world-covering opaque blue basemap tile at zoom 0, and a radar
    // dot at the site's own pixel (the floor's centre).
    let blue_world = || TileLayer {
        zoom: 0,
        tiles: vec![DecodedTile {
            x: 0,
            y: 0,
            side: 8,
            rgba: [0u8, 0, 255, 255].repeat(64),
        }],
    };
    let green_world = TileLayer {
        zoom: 0,
        tiles: vec![DecodedTile {
            x: 0,
            y: 0,
            side: 8,
            rgba: [0u8, 255, 0, 255].repeat(64),
        }],
    };
    let source = source_with_dot(64, (32, 32));

    // Base under radar: the dot's texel is the radar's red, the rest the
    // basemap's blue — not the ground colour.
    let floor = compose_floor(
        &source,
        35.0,
        -97.0,
        (-230.0, 230.0),
        (-230.0, 230.0),
        &blue_world(),
        &TileLayer::empty(),
        &FloorVectors::none(),
    )
    .expect("a composable floor");
    let (col, row) = brightest_red(&floor);
    let mid = FLOOR_TEXELS as usize / 2;
    assert!(
        col.abs_diff(mid) <= 8 && row.abs_diff(mid) <= 8,
        "the radar dot must still land at the centre over a basemap",
    );
    let corner = &floor.rgba[..4];
    assert!(
        corner[2] > 200 && corner[0] < 50,
        "away from the echo the basemap (blue) must show, got {corner:?}",
    );

    // Labels over radar: the same dot texel turns the label layer's
    // green when an opaque label tile covers it.
    let floor = compose_floor(
        &source,
        35.0,
        -97.0,
        (-230.0, 230.0),
        (-230.0, 230.0),
        &blue_world(),
        &green_world,
        &FloorVectors::none(),
    )
    .expect("a composable floor");
    let at = (row * floor.size[0] as usize + col) * 4;
    assert!(
        floor.rgba[at + 1] > 200 && floor.rgba[at] < 50,
        "an opaque label tile must paint over the radar echo, got {:?}",
        &floor.rgba[at..at + 4],
    );
}

/// A square ring of `half_km` kilometres about the site, its corners'
/// geo coordinates produced by the mapping's own forward lines — the
/// fixtures' one way of naming ground, same as every planted dot.
fn geo_square(site_lat: f64, site_lon: f64, half_km: f64) -> Vec<(f64, f64)> {
    let site_lat_rad = site_lat.to_radians();
    let corner = |dx: f64, dy: f64| {
        let lat_rad = site_lat_rad + dy / rustdar_radar::types::EARTH_RADIUS_KM;
        (
            lat_rad.to_degrees(),
            site_lon + dx / (111.32 * lat_rad.cos()),
        )
    };
    vec![
        corner(-half_km, half_km),
        corner(half_km, half_km),
        corner(half_km, -half_km),
        corner(-half_km, -half_km),
    ]
}

/// The vector layers stack where the pane stacks them: outlooks under
/// the radar, alerts over it, label tiles over the alerts.
///
/// The mutation this kills is the order swap — an alert drawn into the
/// under-radar buffer (or an outlook into the over-) leaves every
/// registration test green and quietly draws warnings *behind* the
/// storm they warn about.
#[test]
fn the_vector_layers_stack_where_the_pane_stacks_them() {
    let (site_lat, site_lon) = (35.0, -97.0);
    let ranges = ((-230.0, 230.0), (-230.0, 230.0));
    let source = source_with_dot(64, (32, 32));
    let outlook = FloorShape {
        ring: geo_square(site_lat, site_lon, 40.0),
        fill_rgba: [0, 255, 0, 255],
        stroke_rgba: [0, 0, 0, 0],
    };
    let alert = FloorShape {
        ring: geo_square(site_lat, site_lon, 15.0),
        fill_rgba: [255, 255, 0, 255],
        stroke_rgba: [0, 0, 0, 0],
    };

    // Outlook under radar: the site's dot stays the radar's red; the
    // outlook's green shows beside it, inside its square.
    let floor = compose_floor(
        &source,
        site_lat,
        site_lon,
        ranges.0,
        ranges.1,
        &TileLayer::empty(),
        &TileLayer::empty(),
        &FloorVectors {
            under_radar: vec![outlook.clone()],
            over_radar: Vec::new(),
            range_ring: false,
        },
    )
    .expect("a composable floor");
    let (dot_col, dot_row) = brightest_red(&floor);
    let mid = FLOOR_TEXELS as usize / 2;
    assert!(
        dot_col.abs_diff(mid) <= 8 && dot_row.abs_diff(mid) <= 8,
        "the radar dot must still land at the centre over an outlook",
    );
    let beside = ((dot_row) * FLOOR_TEXELS as usize + dot_col + 20) * 4;
    let px = &floor.rgba[beside..beside + 3];
    assert!(
        px[1] > 200 && px[0] < 50,
        "beside the echo, inside the outlook square, the outlook's green \
             must show under it, got {px:?}",
    );

    // Alert over radar: the same dot texel turns the alert's yellow.
    let floor = compose_floor(
        &source,
        site_lat,
        site_lon,
        ranges.0,
        ranges.1,
        &TileLayer::empty(),
        &TileLayer::empty(),
        &FloorVectors {
            under_radar: vec![outlook.clone()],
            over_radar: vec![alert.clone()],
            range_ring: false,
        },
    )
    .expect("a composable floor");
    let at = (dot_row * FLOOR_TEXELS as usize + dot_col) * 4;
    let px = &floor.rgba[at..at + 3];
    assert!(
        px[0] > 200 && px[1] > 200 && px[2] < 50,
        "an opaque alert must paint over the radar echo, got {px:?}",
    );

    // Label tiles over the alert: the same texel turns the labels' blue.
    let blue_world = TileLayer {
        zoom: 0,
        tiles: vec![DecodedTile {
            x: 0,
            y: 0,
            side: 8,
            rgba: [0u8, 0, 255, 255].repeat(64),
        }],
    };
    let floor = compose_floor(
        &source,
        site_lat,
        site_lon,
        ranges.0,
        ranges.1,
        &TileLayer::empty(),
        &blue_world,
        &FloorVectors {
            under_radar: vec![outlook],
            over_radar: vec![alert],
            range_ring: false,
        },
    )
    .expect("a composable floor");
    let px = &floor.rgba[at..at + 3];
    assert!(
        px[2] > 200 && px[0] < 50,
        "an opaque label tile must paint over the alert, got {px:?}",
    );
}

/// A translucent alert fill tints the ground at its own alpha — the
/// pane's straight-alpha compositing, not an opaque stamp and not a
/// double-blend where scanlines meet.
#[test]
fn an_alert_fill_tints_the_ground_at_its_own_alpha() {
    let shape = FloorShape {
        ring: geo_square(35.0, -97.0, 60.0),
        fill_rgba: [255, 0, 0, 80],
        stroke_rgba: [0, 0, 0, 0],
    };
    let floor = compose_floor(
        &vec![0u8; 64 * 64 * 4],
        35.0,
        -97.0,
        (-230.0, 230.0),
        (-230.0, 230.0),
        &TileLayer::empty(),
        &TileLayer::empty(),
        &FloorVectors {
            under_radar: Vec::new(),
            over_radar: vec![shape],
            range_ring: false,
        },
    )
    .expect("a composable floor");
    // Interior texel: `over(ground, fill)` exactly once.
    let mid = FLOOR_TEXELS as usize / 2;
    let at = (mid * FLOOR_TEXELS as usize + mid) * 4;
    let expected = {
        let alpha = 80.0 / 255.0;
        [
            (255.0 * alpha + f64::from(FLOOR_GROUND_RGBA[0]) * (1.0 - alpha)).round() as u8,
            (f64::from(FLOOR_GROUND_RGBA[1]) * (1.0 - alpha)).round() as u8,
            (f64::from(FLOOR_GROUND_RGBA[2]) * (1.0 - alpha)).round() as u8,
        ]
    };
    for channel in 0..3 {
        assert!(
            floor.rgba[at + channel].abs_diff(expected[channel]) <= 2,
            "the fill must tint the ground once at alpha 80: texel {:?}, \
                 expected about {expected:?}",
            &floor.rgba[at..at + 3],
        );
    }
}

/// The range ring stands 230 km from the site — [`MAX_RANGE_KM`], the
/// radius `render_radar_range_ring` draws — due east and due north, in
/// its own faint grey, and nowhere near the site.
#[test]
fn the_range_ring_stands_at_its_radius() {
    let floor = compose_floor(
        &vec![0u8; 64 * 64 * 4],
        35.0,
        -97.0,
        (-300.0, 300.0),
        (-300.0, 300.0),
        &TileLayer::empty(),
        &TileLayer::empty(),
        &FloorVectors {
            under_radar: Vec::new(),
            over_radar: Vec::new(),
            range_ring: true,
        },
    )
    .expect("a composable floor");
    let side = FLOOR_TEXELS as usize;
    let ringish = |col: usize, row: usize| {
        let at = (row * side + col) * 4;
        let [r, g, b] = [floor.rgba[at], floor.rgba[at + 1], floor.rgba[at + 2]];
        // RANGE_RING_RGBA over the ground: a grey near (58, 59, 62).
        r > FLOOR_GROUND_RGBA[0] + 20 && r.abs_diff(g) < 8 && g.abs_diff(b) < 10
    };
    // 230 km east of a ±300 km box: col ≈ (230+300)/600·512 ≈ 452, on
    // the site's own row ≈ 255; 230 km north mirrors onto row ≈ 59.
    let found_east = (450..=454).any(|col| (252..=259).any(|row| ringish(col, row)));
    let found_north = (57..=62).any(|row| (252..=259).any(|col| ringish(col, row)));
    assert!(
        found_east && found_north,
        "the ring must stand ~452 texels east and ~59 rows north in a \
             ±300 km box (east found: {found_east}, north found: {found_north})",
    );
    let mid = side / 2;
    let at = (mid * side + mid) * 4;
    assert_eq!(
        &floor.rgba[at..at + 4],
        &FLOOR_GROUND_RGBA,
        "the site itself is not on the ring",
    );
}

/// Degenerate inputs are refused, not clamped.
#[test]
fn a_floor_that_cannot_be_registered_is_refused() {
    let source = vec![0u8; 64 * 64 * 4];
    // Not a square image.
    assert!(resample_floor(&source[..60], 35.0, (-1.0, 1.0), (-1.0, 1.0)).is_none());
    // Degenerate ranges and range order.
    assert!(resample_floor(&source, 35.0, (1.0, 1.0), (-1.0, 1.0)).is_none());
    assert!(resample_floor(&source, 35.0, (-1.0, 1.0), (1.0, -1.0)).is_none());
    // A latitude with no finite Mercator row, and a pole, where cos(lat)
    // reaches zero. The raster's half-extent is no longer an input at
    // all — it is the projection's own constant — so there is no wrong
    // extent left to refuse.
    assert!(resample_floor(&source, f64::NAN, (-1.0, 1.0), (-1.0, 1.0)).is_none());
    assert!(resample_floor(&source, 90.0, (-1.0, 1.0), (-1.0, 1.0)).is_none());
}
