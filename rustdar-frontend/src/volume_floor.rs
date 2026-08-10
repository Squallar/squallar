//! The map floor: the 2D pane's ground, resampled onto the 3D box's footprint.
//!
//! # What the floor is, and what it is so far
//!
//! GR2Analyst draws its volume standing on the 2D map — basemap, radar and
//! labels — and that is issue #7's ask: "the floor of the 3d viewer should be
//! the 2d map (literally, the pane content)." What ships here is the honest
//! first cut of that: **the lowest-tilt base reflectivity, exactly as the 2D
//! pane's own rasterizer draws it, over the dark ground colour**, registered
//! to the box footprint. The basemap tiles and the vector overlays are
//! deferred, and the reason is mechanical rather than aesthetic: the tile
//! pipeline decodes straight into egui textures and keeps no CPU bytes, so
//! compositing tiles under the radar needs either an accessor into
//! `egui_wgpu::Renderer`'s texture map or a second decode path — both real
//! designs, neither small, and the registration/compositing machinery built
//! here is exactly what they would plug into.
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
//! the Mercator span). [`resample_floor`] evaluates the same three lines for
//! every floor texel — the box's own `x_range_km`/`y_range_km` are already
//! kilometres east/north of the site, so there is no second projection to
//! disagree with the first: the floor lands where the 2D pane would draw it
//! because it is computed *by* the 2D pane's arithmetic. The one approximation
//! is the raster's own (`φ = φ₀ + dy/R`, spherical), which is the codebase's
//! standing convention (`ImageBounds`, `corners_for`).
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
///   `±max_range_km` about the site.
/// * `max_range_km` — the raster's half-extent, from the same render.
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
    max_range_km: f64,
    site_lat_deg: f64,
    x_range_km: (f64, f64),
    y_range_km: (f64, f64),
) -> Option<FloorImage> {
    let texels = source.len() / 4;
    let side = (texels as f64).sqrt() as usize;
    if side == 0 || side * side * 4 != source.len() {
        return None;
    }
    if !(max_range_km.is_finite() && max_range_km > 0.0) {
        return None;
    }
    if x_range_km.1 <= x_range_km.0 || y_range_km.1 <= y_range_km.0 {
        return None;
    }
    if !site_lat_deg.is_finite() || site_lat_deg.abs() >= 89.0 {
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
        for col in 0..out_w {
            let dx_km =
                x_range_km.0 + (col as f64 + 0.5) / out_w as f64 * (x_range_km.1 - x_range_km.0);
            let px = centre_px + dx_km * cos_correction * px_per_km;
            let echo = bilinear(source, side, px, py);
            rgba.extend_from_slice(&over_ground(echo));
        }
    }
    Some(FloorImage {
        size: [out_w as u32, out_h as u32],
        rgba,
    })
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

/// Straight-alpha `echo` composited over the opaque ground colour, back to
/// bytes. In gamma space, which is the convention every raster in this
/// codebase composites in (egui's own, as the blit's doc lays out).
fn over_ground(echo: [f64; 4]) -> [u8; 4] {
    let alpha = echo[3] / 255.0;
    let mut out = [0u8; 4];
    for channel in 0..3 {
        let ground = f64::from(FLOOR_GROUND_RGBA[channel]);
        out[channel] = (echo[channel] * alpha + ground * (1.0 - alpha)).round() as u8;
    }
    out[3] = 255;
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
        let floor = resample_floor(&source, 230.0, 35.0, (-230.0, 230.0), (-230.0, 230.0))
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
        let floor = resample_floor(&source, 230.0, 35.0, (-230.0, 230.0), (-230.0, 230.0))
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
        let floor = resample_floor(&source, 230.0, 35.0, (60.0, 140.0), (110.0, 190.0))
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
        let floor = resample_floor(&source, 230.0, 35.0, (-230.0, 230.0), (-230.0, 230.0))
            .expect("a resamplable floor");
        assert_eq!(&floor.rgba[..4], &FLOOR_GROUND_RGBA);
        assert!(
            floor.rgba.chunks_exact(4).all(|px| px[3] == 255),
            "the floor must be opaque ground everywhere",
        );
    }

    /// Degenerate inputs are refused, not clamped.
    #[test]
    fn a_floor_that_cannot_be_registered_is_refused() {
        let source = vec![0u8; 64 * 64 * 4];
        // Not a square image.
        assert!(resample_floor(&source[..60], 230.0, 35.0, (-1.0, 1.0), (-1.0, 1.0)).is_none());
        // Degenerate ranges and range order.
        assert!(resample_floor(&source, 230.0, 35.0, (1.0, 1.0), (-1.0, 1.0)).is_none());
        assert!(resample_floor(&source, 230.0, 35.0, (-1.0, 1.0), (1.0, -1.0)).is_none());
        // A max range that divides by zero or is meaningless.
        assert!(resample_floor(&source, 0.0, 35.0, (-1.0, 1.0), (-1.0, 1.0)).is_none());
        assert!(resample_floor(&source, f64::NAN, 35.0, (-1.0, 1.0), (-1.0, 1.0)).is_none());
        // A pole, where cos(lat) reaches zero.
        assert!(resample_floor(&source, 230.0, 90.0, (-1.0, 1.0), (-1.0, 1.0)).is_none());
    }
}
