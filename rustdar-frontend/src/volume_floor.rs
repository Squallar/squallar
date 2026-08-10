//! The map floor: the 2D pane's ground, resampled onto the 3D box's footprint.
//!
//! # What the floor is
//!
//! GR2Analyst draws its volume standing on the 2D map — basemap, radar and
//! labels — and that is issue #7's ask: "the floor of the 3d viewer should be
//! the 2d map (literally, the pane content)." [`compose_floor`] builds that
//! composition: **the basemap tiles the 2D panes draw, the lowest-tilt base
//! reflectivity exactly as the 2D pane's own rasterizer draws it, and the
//! city-label tiles over both**, on the dark ground colour, registered to the
//! box footprint. The tiles come from the panes' own [`HttpsTiles`] sources,
//! which keep each tile's compressed bytes beside the texture for exactly
//! this consumer — the "tile pipeline keeps no CPU bytes" blocker that
//! deferred the first cut was rustdar's own fetch path, and was turned off at
//! its root. The 2D panes' *vector* overlays (counties, warnings, the range
//! ring) are still not part of the floor; they are egui shapes painted
//! through a live `Projector`, and capturing them stays an offscreen-egui
//! design of its own.
//!
//! [`HttpsTiles`]: rustdar_egui::tile_source::HttpsTiles
//!
//! # Registration: one projection, run backwards
//!
//! The 2D raster places a point `(dx, dy)` kilometres east/north of the site
//! at
//!
//! ```text
//! px = W/2 + dx · (cos φ₀ / cos φ) · px_per_km
//! py = (mercᵧ(max_lat) − mercᵧ(φ)) · W / (mercᵧ(max_lat) − mercᵧ(min_lat))
//! φ  = φ₀ + dy / R
//! ```
//!
//! (`render::MercatorProjection::render_gate`, with `ImageBounds` supplying
//! the Mercator span). [`compose_floor`] evaluates the same three lines for
//! every floor texel — the box's own `x_range_km`/`y_range_km` are already
//! kilometres east/north of the site, so there is no second projection to
//! disagree with the first: the floor lands where the 2D pane would draw it
//! because it is computed *by* the 2D pane's arithmetic. The one approximation
//! is the raster's own (`φ = φ₀ + dy/R`, spherical), which is the codebase's
//! standing convention (`ImageBounds`, `corners_for`).
//!
//! The **tiles** read through the same mapping, not a second one. The
//! raster's x axis is linear in longitude (`ImageBounds` spans
//! `±MAX_RANGE_KM / (111.32 cos φ₀)` degrees over the image), and
//! substituting that span into the `px` line above gives the raster's own
//! longitude for a floor texel:
//!
//! ```text
//! λ = λ₀ + dx / (111.32 · cos φ)
//! ```
//!
//! — the same `cos_correction` reappearing as algebra, not a new convention.
//! `(λ, φ)` then indexes the slippy-tile pyramid the standard way
//! (`x = (λ+180)/360 · 2^z`, `y = (1 − mercᵧ(φ)/π)/2 · 2^z`, the same
//! [`mercator_y`]), so a tile pixel and a radar gate that name the same
//! ground land on the same texel; `a_tile_pixel_and_a_radar_gate_at_the_same_
//! ground_land_on_the_same_texel` pins that agreement through both consumers.
//!
//! # Refresh
//!
//! A floor is rendered when a voxel build completes — the same cadence the
//! volume itself refreshes at, once per sealed sweep per site — and never per
//! frame. The store holds one per `(site, region)` and replaces it in place;
//! the GPU upload is keyed by the floor's id and reused until it changes.

use std::f64::consts::PI;

/// Texels along each edge of the floor image.
///
/// 512 over a footprint of at most 460 km is 0.9 km per texel — the raster
/// underneath resolves ~0.22 km, but the floor is viewed obliquely at pane
/// scale where 512 reads clean, and one floor is 1 MiB against the grid's 8.
pub const FLOOR_TEXELS: u32 = 512;

/// The ground where no echo painted: near-black, the colour GR's floor and
/// the dark basemap read as. Opaque, because the floor is ground — a
/// transparent gap in it would read as a hole in the earth.
pub const FLOOR_GROUND_RGBA: [u8; 4] = [16, 18, 22, 255];

/// A floor ready for upload: straight, gamma-encoded RGBA, row 0 the box
/// footprint's north edge.
#[derive(Clone, Debug, PartialEq)]
pub struct FloorImage {
    /// Texels along x (east) and y (north-to-south rows).
    pub size: [u32; 2],
    /// `size[0] * size[1] * 4` bytes, row-major from the north-west corner.
    pub rgba: Vec<u8>,
}

/// `ln(tan(π/4 + φ/2))` — Web Mercator's y, exactly as
/// `rustdar_radar::types::lat_rad_to_mercator_y` computes it. Restated here
/// because that function is `pub(crate)` to its own crate; the formula is the
/// projection's definition, so the two cannot drift without one of them
/// ceasing to be Web Mercator.
fn mercator_y(lat_rad: f64) -> f64 {
    (PI / 4.0 + lat_rad / 2.0).tan().ln()
}

/// Resample the site-centred radar raster onto the box footprint.
///
/// * `source` — the raster's RGBA bytes, a square image as
///   `render_radar_to_image` produces: linear in longitude and Mercator y,
///   **`±MAX_RANGE_KM` about the site** — the raster's half-extent is
///   [`rustdar_radar::types::MAX_RANGE_KM`], read directly below rather than
///   taken as a parameter. `ImageBounds::from_radar_site` builds every raster
///   at that constant unconditionally, and the render's *returned*
///   `max_range_km` is a different number — the product's **data reach**
///   (super-res reflectivity gates run to ~460 km), kept for the range ring
///   and the hover readout. Feeding that reach in as the half-extent halves
///   the sampler's pixels-per-km and zooms the floor in 2×, which is exactly
///   the shipped bug the 2026-08-09 report's screenshot shows: strong cores
///   drawn at twice their true distance from the site, under an empty sky.
///   With the constant read at the definition, no caller can repeat it.
/// * `site_lat_deg` — the site's latitude; the raster's projection constants
///   derive from it and nothing needs the longitude, because the box's x is
///   already kilometres east of the site.
/// * `x_range_km`, `y_range_km` — the box footprint, kilometres east/north of
///   the site: `VoxelGrid::x_range_km()`/`y_range_km()`, so the floor is
///   registered to the grid actually built rather than to a re-derivation.
///
/// `None` for a source that is not a square RGBA image or a degenerate
/// footprint — both impossible from the production callers, and refused
/// rather than clamped for the usual reason: every arm below divides.
pub fn resample_floor(
    source: &[u8],
    site_lat_deg: f64,
    x_range_km: (f64, f64),
    y_range_km: (f64, f64),
) -> Option<FloorImage> {
    // The site longitude only places tiles, and there are none here.
    compose_floor(
        source,
        site_lat_deg,
        0.0,
        x_range_km,
        y_range_km,
        &TileLayer::empty(),
        &TileLayer::empty(),
    )
}

/// One decoded map tile: its slippy-pyramid coordinates at the layer's zoom,
/// and its straight-alpha RGBA pixels.
pub struct DecodedTile {
    /// Slippy x — west to east, `0..2^zoom`.
    pub x: u32,
    /// Slippy y — north to south, `0..2^zoom`.
    pub y: u32,
    /// Pixels along each edge (tiles are square; 256 for the shipped sources).
    pub side: u32,
    /// `side * side * 4` straight, gamma-encoded RGBA bytes, row 0 the tile's
    /// north edge.
    pub rgba: Vec<u8>,
}

/// A zoom level's worth of decoded tiles for one floor composite.
pub struct TileLayer {
    pub zoom: u8,
    pub tiles: Vec<DecodedTile>,
}

impl TileLayer {
    /// No tiles at all — the layer composites as nothing.
    pub fn empty() -> Self {
        TileLayer {
            zoom: 0,
            tiles: Vec::new(),
        }
    }
}

/// A zoom level's worth of **compressed** tiles, as gathered on the frame
/// thread from the panes' own [`HttpsTiles`] caches — `(slippy x, slippy y,
/// the tile's PNG bytes)`. Decoding is [`TileBytesLayer::decode`], run in the
/// floor job's delivery rather than here, so the frame thread hands over
/// `Arc`s and the decode cost lands where the resample's already does.
///
/// [`HttpsTiles`]: rustdar_egui::tile_source::HttpsTiles
pub struct TileBytesLayer {
    pub zoom: u8,
    pub tiles: Vec<(u32, u32, std::sync::Arc<Vec<u8>>)>,
}

impl TileBytesLayer {
    /// No tiles — decodes to [`TileLayer::empty`].
    pub fn empty() -> Self {
        TileBytesLayer {
            zoom: 0,
            tiles: Vec::new(),
        }
    }

    /// Decode every tile to straight RGBA. A tile that does not decode is
    /// dropped with a log line rather than failing the floor: the map under
    /// the box missing one tile beats a box standing on nothing.
    pub fn decode(&self) -> TileLayer {
        TileLayer {
            zoom: self.zoom,
            tiles: self
                .tiles
                .iter()
                .filter_map(|(x, y, bytes)| match image::load_from_memory(bytes) {
                    Ok(decoded) => {
                        let rgba = decoded.to_rgba8();
                        let (w, h) = rgba.dimensions();
                        if w != h || w == 0 {
                            log::warn!("floor tile {x}/{y} is {w}x{h}, not square; dropped");
                            return None;
                        }
                        Some(DecodedTile {
                            x: *x,
                            y: *y,
                            side: w,
                            rgba: rgba.into_raw(),
                        })
                    }
                    Err(error) => {
                        log::warn!("floor tile {x}/{y} failed to decode: {error}");
                        None
                    }
                })
                .collect(),
        }
    }
}

/// [`resample_floor`], with the 2D map's tile layers under and over the
/// radar: ground colour, then the basemap tiles, then the radar echo, then
/// the city-label tiles — the 2D pane's own stacking order.
///
/// A tile that is missing from `base`/`labels` simply contributes nothing;
/// the ground colour (and whatever tiles are present) show instead, and the
/// caller re-composes when more tiles land. Layers sample bilinearly within
/// each tile, clamped at tile edges — the half-texel seam this admits is
/// under the floor's own texel size at the zoom [`floor_tile_zoom`] picks.
pub fn compose_floor(
    source: &[u8],
    site_lat_deg: f64,
    site_lon_deg: f64,
    x_range_km: (f64, f64),
    y_range_km: (f64, f64),
    base: &TileLayer,
    labels: &TileLayer,
) -> Option<FloorImage> {
    let max_range_km = rustdar_radar::types::MAX_RANGE_KM;
    let texels = source.len() / 4;
    let side = (texels as f64).sqrt() as usize;
    if side == 0 || side * side * 4 != source.len() {
        return None;
    }
    if x_range_km.1 <= x_range_km.0 || y_range_km.1 <= y_range_km.0 {
        return None;
    }
    if !(site_lat_deg.is_finite() && site_lon_deg.is_finite()) || site_lat_deg.abs() >= 89.0 {
        return None;
    }

    // The raster's own projection constants, as `MercatorProjection` and
    // `ImageBounds::from_radar_site` build them.
    let site_lat_rad = site_lat_deg.to_radians();
    let cos_site_lat = site_lat_rad.cos();
    let px_per_km = side as f64 / (2.0 * max_range_km);
    let centre_px = side as f64 / 2.0;
    let lat_deg_per_km = 1.0 / 111.32;
    let max_lat = site_lat_deg + max_range_km * lat_deg_per_km;
    let min_lat = site_lat_deg - max_range_km * lat_deg_per_km;
    let merc_top = mercator_y(max_lat.to_radians());
    let merc_scale = side as f64 / (merc_top - mercator_y(min_lat.to_radians()));

    let out_w = FLOOR_TEXELS as usize;
    let out_h = FLOOR_TEXELS as usize;
    let mut rgba = Vec::with_capacity(out_w * out_h * 4);
    for row in 0..out_h {
        // Row 0 is the footprint's north edge.
        let dy_km =
            y_range_km.1 - (row as f64 + 0.5) / out_h as f64 * (y_range_km.1 - y_range_km.0);
        let lat_rad = site_lat_rad + dy_km / rustdar_radar::types::EARTH_RADIUS_KM;
        let cos_correction = cos_site_lat / lat_rad.cos();
        let py = (merc_top - mercator_y(lat_rad)) * merc_scale;
        // This row's slippy y is shared by every column: it depends only on
        // latitude. `merc_y` is in `(-π, π)`, π at the north pole.
        let merc_y = mercator_y(lat_rad);
        for col in 0..out_w {
            let dx_km =
                x_range_km.0 + (col as f64 + 0.5) / out_w as f64 * (x_range_km.1 - x_range_km.0);
            let px = centre_px + dx_km * cos_correction * px_per_km;
            // The raster's own longitude for this texel — the module doc
            // derives this line from the `px` line above; it is the same
            // mapping, not a second one.
            let lon_deg = site_lon_deg + dx_km / (111.32 * lat_rad.cos());

            let mut texel = [
                f64::from(FLOOR_GROUND_RGBA[0]),
                f64::from(FLOOR_GROUND_RGBA[1]),
                f64::from(FLOOR_GROUND_RGBA[2]),
            ];
            over(&mut texel, sample_layer(base, lon_deg, merc_y));
            over(&mut texel, bilinear(source, side, px, py));
            over(&mut texel, sample_layer(labels, lon_deg, merc_y));
            rgba.extend_from_slice(&[
                texel[0].round() as u8,
                texel[1].round() as u8,
                texel[2].round() as u8,
                255,
            ]);
        }
    }
    Some(FloorImage {
        size: [out_w as u32, out_h as u32],
        rgba,
    })
}

/// `layer` sampled at a longitude and Mercator y, straight RGBA — transparent
/// where the layer holds no tile or the tile's own pixel is transparent.
fn sample_layer(layer: &TileLayer, lon_deg: f64, merc_y: f64) -> [f64; 4] {
    if layer.tiles.is_empty() {
        return [0.0; 4];
    }
    let n = f64::from(1u32 << layer.zoom.min(31));
    let tile_x = (lon_deg + 180.0) / 360.0 * n;
    let tile_y = (1.0 - merc_y / PI) / 2.0 * n;
    for tile in &layer.tiles {
        let fx = tile_x - f64::from(tile.x);
        let fy = tile_y - f64::from(tile.y);
        if !(0.0..1.0).contains(&fx) || !(0.0..1.0).contains(&fy) {
            continue;
        }
        let side = tile.side as usize;
        if side * side * 4 != tile.rgba.len() {
            continue;
        }
        // Clamp the bilinear kernel inside this tile: the neighbour's edge
        // pixels are a different allocation, and half a tile pixel of clamp
        // at the seam is under the floor's own texel size.
        let px = (fx * side as f64).clamp(0.5, side as f64 - 0.5);
        let py = (fy * side as f64).clamp(0.5, side as f64 - 0.5);
        return bilinear(&tile.rgba, side, px, py);
    }
    [0.0; 4]
}

/// Straight-alpha `layer` composited over opaque `under`, both gamma-encoded —
/// the same convention every raster in this codebase composites in (egui's
/// own, as the blit's doc lays out).
fn over(under: &mut [f64; 3], layer: [f64; 4]) {
    let alpha = layer[3] / 255.0;
    for (channel, ground) in under.iter_mut().enumerate() {
        *ground = layer[channel] * alpha + *ground * (1.0 - alpha);
    }
}

/// The slippy zoom whose tiles resolve about one tile pixel per floor texel
/// over this footprint, clamped to what the source can serve.
///
/// At `2^z · 256` pixels around the world, the footprint's longitude span
/// covers `span/360` of that; solving for the floor's own [`FLOOR_TEXELS`]
/// gives `z = log2(360 · FLOOR_TEXELS / (256 · span))`. The default 460 km
/// box at mid-latitudes lands at z7 — two-and-a-bit tiles across, city
/// labels at their native size, which is the GR look.
pub fn floor_tile_zoom(site_lat_deg: f64, x_range_km: (f64, f64), max_zoom: u8) -> u8 {
    let width_km = (x_range_km.1 - x_range_km.0).max(1.0);
    let cos_lat = site_lat_deg.to_radians().cos().max(0.01);
    let lon_span_deg = width_km / (111.32 * cos_lat);
    let z = (360.0 * f64::from(FLOOR_TEXELS) / (256.0 * lon_span_deg)).log2();
    (z.round().max(0.0) as u8).min(max_zoom)
}

/// Every slippy tile id at `zoom` the footprint touches, as `(x, y)` pairs —
/// through the same corner mapping the composite samples with.
///
/// Longitude reach is widest at the row furthest from the equator (the
/// `cos φ` in the mapping), so both y extremes are evaluated and the wider
/// wins. Bounded by construction: [`floor_tile_zoom`] picks a zoom where the
/// box is a few tiles across, and the count is clamped to an 8×8 window
/// against a degenerate request.
pub fn floor_tile_ids(
    site_lat_deg: f64,
    site_lon_deg: f64,
    x_range_km: (f64, f64),
    y_range_km: (f64, f64),
    zoom: u8,
) -> Vec<(u32, u32)> {
    let site_lat_rad = site_lat_deg.to_radians();
    let lat_at = |dy_km: f64| site_lat_rad + dy_km / rustdar_radar::types::EARTH_RADIUS_KM;
    let lon_at = |dx_km: f64, lat_rad: f64| site_lon_deg + dx_km / (111.32 * lat_rad.cos());

    let n = f64::from(1u32 << zoom.min(31));
    let max_index = (1u64 << zoom.min(31)) - 1;
    let tile_x = |lon: f64| ((lon + 180.0) / 360.0 * n).floor();
    let tile_y = |lat_rad: f64| ((1.0 - mercator_y(lat_rad) / PI) / 2.0 * n).floor();

    let (lat_s, lat_n) = (lat_at(y_range_km.0), lat_at(y_range_km.1));
    let mut x_lo = f64::INFINITY;
    let mut x_hi = f64::NEG_INFINITY;
    for lat in [lat_s, lat_n] {
        for dx in [x_range_km.0, x_range_km.1] {
            let x = tile_x(lon_at(dx, lat));
            x_lo = x_lo.min(x);
            x_hi = x_hi.max(x);
        }
    }
    let clamp_to = |v: f64| (v.max(0.0) as u64).min(max_index) as u32;
    let (x_lo, x_hi) = (clamp_to(x_lo), clamp_to(x_hi));
    // Slippy y grows southwards, so the north edge is the smaller index.
    let (y_lo, y_hi) = (clamp_to(tile_y(lat_n)), clamp_to(tile_y(lat_s)));

    let mut ids = Vec::new();
    for y in y_lo..=y_hi.min(y_lo + 7) {
        for x in x_lo..=x_hi.min(x_lo + 7) {
            ids.push((x, y));
        }
    }
    ids
}

/// `source` bilinearly sampled at `(px, py)`, straight RGBA. Outside the
/// image is transparent — beyond the raster's range there is no echo, and the
/// ground colour supplies the rest.
fn bilinear(source: &[u8], side: usize, px: f64, py: f64) -> [f64; 4] {
    let sample = |ix: i64, iy: i64| -> [f64; 4] {
        if ix < 0 || iy < 0 || ix >= side as i64 || iy >= side as i64 {
            return [0.0; 4];
        }
        let at = (iy as usize * side + ix as usize) * 4;
        [
            f64::from(source[at]),
            f64::from(source[at + 1]),
            f64::from(source[at + 2]),
            f64::from(source[at + 3]),
        ]
    };
    let fx = px - 0.5;
    let fy = py - 0.5;
    let ix = fx.floor();
    let iy = fy.floor();
    let tx = fx - ix;
    let ty = fy - iy;
    let (ix, iy) = (ix as i64, iy as i64);
    let mut out = [0.0f64; 4];
    for (channel, slot) in out.iter_mut().enumerate() {
        let top = sample(ix, iy)[channel] * (1.0 - tx) + sample(ix + 1, iy)[channel] * tx;
        let bottom =
            sample(ix, iy + 1)[channel] * (1.0 - tx) + sample(ix + 1, iy + 1)[channel] * tx;
        *slot = top * (1.0 - ty) + bottom * ty;
    }
    out
}

#[cfg(test)]
mod tests {
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
        let floor = resample_floor(&source, 35.0, (60.0, 140.0), (110.0, 190.0))
            .expect("a resamplable floor");
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
    /// independent forward routes — compose, and return how far apart the
    /// two dots landed on the floor, in texels.
    fn tile_vs_gate_disagreement(dx_km: f64, dy_km: f64) -> usize {
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
        best_green
            .0
            .abs_diff(red_col)
            .max(best_green.1.abs_diff(red_row))
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
            let apart = tile_vs_gate_disagreement(dx_km, dy_km);
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
        )
        .expect("a composable floor");
        let at = (row * floor.size[0] as usize + col) * 4;
        assert!(
            floor.rgba[at + 1] > 200 && floor.rgba[at] < 50,
            "an opaque label tile must paint over the radar echo, got {:?}",
            &floor.rgba[at..at + 4],
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
}
