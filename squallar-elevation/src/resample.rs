//! Terrain-RGB tiles in, one [`HeightField`] on the volume box's post grid out.
//!
//! Three things here are load-bearing and each has cost a project somewhere.
//!
//! **1. [`squallar_geo::great_circle_destination`] is called directly.** It is
//! the *forward* map — box kilometres to a place on the ground — and it is the
//! same function the volume box is built with. No inverse is written here, so
//! there is no second projection to disagree with the first. The obvious
//! shortcut, a flat "degrees per kilometre" map about the site, is **30.8 km
//! out at the corners of a default 920 km box over Colorado** (computed at
//! 39°N/106°W, this work unit); `tests/resample_oracle.rs` asserts that gap
//! rather than describing it.
//!
//! **2. The tiles are assembled into one contiguous pixel plane before
//! anything is sampled.** Terrain-RGB tiles are edge-sharing grids whose pixel
//! centres do not coincide across a boundary, so per-tile bilinear with a
//! per-tile clamp puts a visible seam at every tile edge. This is why the job
//! takes all its tiles at once rather than streaming them.
//!
//! **3. Bilinear runs on unpacked metres, never on the packed bytes.** The
//! encoding is a base-256 positional number, so averaging the digits ignores
//! every carry between them — the builder measured **max error 3289.7 m** for
//! exactly that mistake at a single 2× reduction
//! (`tools/squallar-terrain/src/raster.rs`, "THE OVERVIEW TRAP").

use std::f64::consts::PI;

use crate::height::{HeightField, encode_height_m};
use crate::trgb;

/// Everything that can go wrong between a bag of tile bodies and a height field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ElevationError {
    /// The cover names no tiles, or a post grid has a zero side.
    Empty,
    /// A tile the cover names was not supplied.
    MissingTile { x: u32, y: u32 },
    /// A tile was supplied that the cover does not name.
    UnexpectedTile { x: u32, y: u32 },
    /// The PNG did not decode.
    Undecodable { x: u32, y: u32, reason: String },
    /// The PNG decoded to something other than 8-bit RGB.
    ///
    /// Not a nicety: a 16-bit PNG converted down to `Rgb8` would decode to
    /// heights that look plausible and are wrong by kilometres, and this
    /// encoding is stored losslessly precisely so that cannot happen.
    NotEightBitRgb { x: u32, y: u32, found: String },
    /// Tiles in one cover disagreed about their pixel size, or were not square.
    TileSize {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    /// The box wraps the antimeridian, which this cover arithmetic does not do.
    CrossesAntimeridian,
}

impl std::fmt::Display for ElevationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "an empty tile cover or post grid"),
            Self::MissingTile { x, y } => write!(f, "tile ({x}, {y}) was not supplied"),
            Self::UnexpectedTile { x, y } => write!(f, "tile ({x}, {y}) is outside the cover"),
            Self::Undecodable { x, y, reason } => {
                write!(f, "tile ({x}, {y}) did not decode: {reason}")
            }
            Self::NotEightBitRgb { x, y, found } => write!(
                f,
                "tile ({x}, {y}) is {found}, not 8-bit RGB; terrain-RGB is \
                 stored losslessly and a converted copy decodes to wrong heights"
            ),
            Self::TileSize {
                x,
                y,
                width,
                height,
            } => write!(f, "tile ({x}, {y}) is {width}x{height}"),
            Self::CrossesAntimeridian => {
                write!(f, "the box crosses the antimeridian, which is unsupported")
            }
        }
    }
}

impl std::error::Error for ElevationError {}

/// The rectangle of tiles one box needs, at one zoom, inclusive at both ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileCover {
    pub zoom: u8,
    pub tile_px: u32,
    pub tx0: u32,
    pub ty0: u32,
    pub tx1: u32,
    pub ty1: u32,
}

impl TileCover {
    /// Tiles across.
    pub fn tiles_x(&self) -> u32 {
        self.tx1 - self.tx0 + 1
    }

    /// Tiles down.
    pub fn tiles_y(&self) -> u32 {
        self.ty1 - self.ty0 + 1
    }

    /// Tiles in the whole rectangle.
    pub fn len(&self) -> usize {
        self.tiles_x() as usize * self.tiles_y() as usize
    }

    /// Never true for a cover this crate produces; present so the type reads
    /// like a collection rather than surprising a caller.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether `(x, y)` is one of the tiles this cover names.
    pub fn contains(&self, x: u32, y: u32) -> bool {
        (self.tx0..=self.tx1).contains(&x) && (self.ty0..=self.ty1).contains(&y)
    }

    /// Every tile address in the rectangle, row-major from the north-west.
    pub fn addresses(&self) -> impl Iterator<Item = (u32, u32)> + use<> {
        let (tx0, tx1, ty0, ty1) = (self.tx0, self.tx1, self.ty0, self.ty1);
        (ty0..=ty1).flat_map(move |y| (tx0..=tx1).map(move |x| (x, y)))
    }
}

/// Web Mercator global pixel coordinates of a point, at `zoom`.
///
/// The pair [`squallar_geo::lat_rad_to_mercator_y`] and
/// [`squallar_geo::mercator_y_to_lat_rad`] rather than a fourth spelling of the
/// projection: they are the documented forward/inverse pair, pinned to each
/// other in that crate's own round-trip test. `the_planes_own_tile_addresses_agree_with_the_library`
/// holds this against `squallar_geo::lat_to_tile_y`'s `asinh(tan)` spelling, so
/// the plane reads the pixels of the tiles it asked for.
fn global_px(lat: f64, lon: f64, zoom: u8, tile_px: u32) -> (f64, f64) {
    let world = f64::from(tile_px) * 2f64.powi(i32::from(zoom));
    let x = (lon + 180.0) / 360.0 * world;
    let y = (1.0 - squallar_geo::lat_rad_to_mercator_y(lat.to_radians()) / PI) / 2.0 * world;
    (x, y)
}

/// The centre of post `(i, j)` in box kilometres, east then north.
///
/// Post **centres**: the field covers the box evenly and the outer posts sit
/// half a cell inside the edges. [`HeightField::post_center_km`] is the same
/// rule read back off a finished field.
pub fn post_center_km(
    x_km: (f64, f64),
    y_km: (f64, f64),
    posts: [u32; 2],
    i: u32,
    j: u32,
) -> (f64, f64) {
    (
        x_km.0 + (f64::from(i) + 0.5) * (x_km.1 - x_km.0) / f64::from(posts[0]),
        y_km.0 + (f64::from(j) + 0.5) * (y_km.1 - y_km.0) / f64::from(posts[1]),
    )
}

/// Where post `(i, j)` actually is on the ground, as `(lat, lon)` in degrees.
///
/// The one projection in this crate, and it is the forward one.
pub fn post_geo(
    site: (f64, f64),
    x_km: (f64, f64),
    y_km: (f64, f64),
    posts: [u32; 2],
    i: u32,
    j: u32,
) -> (f64, f64) {
    let (x, y) = post_center_km(x_km, y_km, posts, i, j);
    let range_km = x.hypot(y);
    let bearing_deg = x.atan2(y).to_degrees();
    squallar_geo::great_circle_destination(site.0, site.1, bearing_deg, range_km)
}

/// The tiles a box's post grid needs at `zoom`, with the one-pixel margin
/// bilinear interpolation reads outside the outermost post.
///
/// The extremes of latitude and longitude over a great-circle box are attained
/// on its boundary, so only the boundary posts are walked.
pub fn cover_for(
    site: (f64, f64),
    x_km: (f64, f64),
    y_km: (f64, f64),
    posts: [u32; 2],
    zoom: u8,
    tile_px: u32,
) -> Result<TileCover, ElevationError> {
    if posts[0] == 0 || posts[1] == 0 || tile_px == 0 {
        return Err(ElevationError::Empty);
    }
    let world = f64::from(tile_px) * 2f64.powi(i32::from(zoom));
    let last = 2u32.saturating_pow(u32::from(zoom)).saturating_sub(1);

    let mut lo = (f64::MAX, f64::MAX);
    let mut hi = (f64::MIN, f64::MIN);
    let mut lon_lo = f64::MAX;
    let mut lon_hi = f64::MIN;
    let mut visit = |i: u32, j: u32| {
        let (lat, lon) = post_geo(site, x_km, y_km, posts, i, j);
        lon_lo = lon_lo.min(lon);
        lon_hi = lon_hi.max(lon);
        let (px, py) = global_px(lat, lon, zoom, tile_px);
        lo = (lo.0.min(px), lo.1.min(py));
        hi = (hi.0.max(px), hi.1.max(py));
    };
    for i in 0..posts[0] {
        visit(i, 0);
        visit(i, posts[1] - 1);
    }
    for j in 0..posts[1] {
        visit(0, j);
        visit(posts[0] - 1, j);
    }

    // `great_circle_destination` returns `site_lon + Δlon` and does **not**
    // wrap, so a box straddling the antimeridian comes back as longitudes past
    // ±180 rather than as a 360° span. Either shape is refused: unwrapped
    // longitudes would clamp to the world's edge column and silently sample the
    // wrong pixels, and a normalised one would name a pixel range covering the
    // whole planet.
    if lon_lo < -180.0 || lon_hi > 180.0 || lon_hi - lon_lo > 180.0 {
        return Err(ElevationError::CrossesAntimeridian);
    }

    // One pixel of margin each way: the outermost post's bilinear cell reaches
    // half a pixel past it, and the extra half keeps a post landing exactly on
    // a pixel centre from depending on a floating-point tie.
    let clamp_px = |v: f64| v.clamp(0.0, world - 1.0);
    let x0 = (clamp_px(lo.0 - 1.0) / f64::from(tile_px)).floor() as u32;
    let x1 = (clamp_px(hi.0 + 1.0) / f64::from(tile_px)).floor() as u32;
    let y0 = (clamp_px(lo.1 - 1.0) / f64::from(tile_px)).floor() as u32;
    let y1 = (clamp_px(hi.1 + 1.0) / f64::from(tile_px)).floor() as u32;

    Ok(TileCover {
        zoom,
        tile_px,
        tx0: x0.min(last),
        ty0: y0.min(last),
        tx1: x1.min(last),
        ty1: y1.min(last),
    })
}

/// One contiguous plane of unpacked metres, assembled from a tile rectangle.
#[derive(Clone, Debug)]
pub struct TilePlane {
    cover: TileCover,
    width_px: u32,
    height_px: u32,
    /// Metres, row-major from the plane's north-west pixel.
    heights_m: Vec<f32>,
}

impl TilePlane {
    /// Decode every tile the cover names and lay them out edge to edge.
    ///
    /// Every tile is required: a hole would read as whatever the surrounding
    /// pixels happen to be, which is a silent wrong answer rather than a
    /// failure.
    pub fn assemble(cover: TileCover, tiles: &[(u32, u32, &[u8])]) -> Result<Self, ElevationError> {
        if cover.tx1 < cover.tx0 || cover.ty1 < cover.ty0 || cover.tile_px == 0 {
            return Err(ElevationError::Empty);
        }
        for (x, y, _) in tiles {
            if !cover.contains(*x, *y) {
                return Err(ElevationError::UnexpectedTile { x: *x, y: *y });
            }
        }

        let tile_px = cover.tile_px;
        let width_px = cover.tiles_x() * tile_px;
        let height_px = cover.tiles_y() * tile_px;
        let mut heights_m = vec![f32::NAN; width_px as usize * height_px as usize];

        for (x, y) in cover.addresses() {
            let (_, _, png) = tiles
                .iter()
                .find(|(tx, ty, _)| *tx == x && *ty == y)
                .ok_or(ElevationError::MissingTile { x, y })?;
            let rgb = decode_rgb8(x, y, png)?;
            if rgb.width() != tile_px || rgb.height() != tile_px {
                return Err(ElevationError::TileSize {
                    x,
                    y,
                    width: rgb.width(),
                    height: rgb.height(),
                });
            }
            let ox = (x - cover.tx0) * tile_px;
            let oy = (y - cover.ty0) * tile_px;
            for row in 0..tile_px {
                let dst = (oy + row) as usize * width_px as usize + ox as usize;
                for col in 0..tile_px {
                    let p = rgb.get_pixel(col, row).0;
                    // Unpacked HERE, once, before anything interpolates: see
                    // this module's point 3.
                    heights_m[dst + col as usize] = trgb::unpack([p[0], p[1], p[2]]) as f32;
                }
            }
        }

        Ok(Self {
            cover,
            width_px,
            height_px,
            heights_m,
        })
    }

    /// The cover this plane was assembled over.
    pub fn cover(&self) -> TileCover {
        self.cover
    }

    /// Pixels across and down.
    pub fn size_px(&self) -> (u32, u32) {
        (self.width_px, self.height_px)
    }

    /// The metres at plane pixel `(col, row)`. `None` off the plane.
    pub fn pixel_m(&self, col: u32, row: u32) -> Option<f64> {
        if col >= self.width_px || row >= self.height_px {
            return None;
        }
        Some(f64::from(
            self.heights_m[row as usize * self.width_px as usize + col as usize],
        ))
    }

    /// Bilinear metres at a point, clamped at the plane's edges.
    ///
    /// The clamp only ever engages at the world's edge or a fraction of a pixel
    /// outside the assembled rectangle, because [`cover_for`] takes a
    /// one-pixel margin. It is *not* a per-tile clamp; that is the seam this
    /// module's point 2 exists to avoid.
    pub fn sample_height_m(&self, lat: f64, lon: f64) -> f64 {
        let (gx, gy) = global_px(lat, lon, self.cover.zoom, self.cover.tile_px);
        // Pixel *centres* carry the samples, so a pixel's centre is at index
        // + 0.5 and the interpolation coordinate is half a pixel back.
        let px = gx - f64::from(self.cover.tx0 * self.cover.tile_px) - 0.5;
        let py = gy - f64::from(self.cover.ty0 * self.cover.tile_px) - 0.5;

        let x0 = px.floor();
        let y0 = py.floor();
        let fx = px - x0;
        let fy = py - y0;
        let cx = |v: f64| v.clamp(0.0, f64::from(self.width_px - 1)) as u32;
        let cy = |v: f64| v.clamp(0.0, f64::from(self.height_px - 1)) as u32;
        let (i0, i1) = (cx(x0), cx(x0 + 1.0));
        let (j0, j1) = (cy(y0), cy(y0 + 1.0));

        let at = |i: u32, j: u32| {
            f64::from(self.heights_m[j as usize * self.width_px as usize + i as usize])
        };
        let top = at(i0, j0) * (1.0 - fx) + at(i1, j0) * fx;
        let bottom = at(i0, j1) * (1.0 - fx) + at(i1, j1) * fx;
        top * (1.0 - fy) + bottom * fy
    }

    /// Resample this plane onto a box's post grid.
    pub fn resample(
        &self,
        site: (f64, f64),
        x_km: (f64, f64),
        y_km: (f64, f64),
        posts: [u32; 2],
    ) -> Result<HeightField, ElevationError> {
        if posts[0] == 0 || posts[1] == 0 {
            return Err(ElevationError::Empty);
        }
        let mut samples = Vec::with_capacity(posts[0] as usize * posts[1] as usize);
        for j in 0..posts[1] {
            for i in 0..posts[0] {
                let (lat, lon) = post_geo(site, x_km, y_km, posts, i, j);
                samples.push(encode_height_m(self.sample_height_m(lat, lon)));
            }
        }
        Ok(HeightField {
            site,
            x_km,
            y_km,
            posts,
            samples,
        })
    }
}

/// Decode one tile body, insisting on 8-bit RGB.
fn decode_rgb8(x: u32, y: u32, png: &[u8]) -> Result<image::RgbImage, ElevationError> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).map_err(|e| {
        ElevationError::Undecodable {
            x,
            y,
            reason: e.to_string(),
        }
    })?;
    match img {
        image::DynamicImage::ImageRgb8(buf) => Ok(buf),
        other => Err(ElevationError::NotEightBitRgb {
            x,
            y,
            found: format!("{:?}", other.color()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plane's own Web Mercator and the library's integer tile addressing
    /// agree, so the pixels it indexes belong to the tiles it asked for.
    ///
    /// Two spellings of the same projection live in `squallar-geo`:
    /// `lat_rad_to_mercator_y` and `lat_to_tile_y`'s `asinh(tan)`. This holds
    /// them together at the addresses this crate uses.
    #[test]
    fn the_planes_own_tile_addresses_agree_with_the_library() {
        for zoom in [0u8, 4, 8, 11, 12] {
            for lat in [-80.0_f64, -45.0, -0.5, 0.0, 0.5, 39.0, 60.0, 80.0] {
                for lon in [-179.0_f64, -106.0, -0.5, 0.0, 0.5, 120.0, 179.0] {
                    let (px, py) = global_px(lat, lon, zoom, 256);
                    assert_eq!(
                        (px / 256.0).floor() as u32,
                        squallar_geo::lon_to_tile_x(lon, zoom),
                        "x at z{zoom} {lat},{lon}"
                    );
                    assert_eq!(
                        (py / 256.0).floor() as u32,
                        squallar_geo::lat_to_tile_y(lat, zoom),
                        "y at z{zoom} {lat},{lon}"
                    );
                }
            }
        }
    }

    /// Every post the resample will read falls inside the cover, margin
    /// included — otherwise the sampler's edge clamp would silently stand in
    /// for a tile that was never fetched.
    #[test]
    fn the_cover_holds_every_post_and_its_bilinear_neighbourhood() {
        let site = (39.0, -106.0);
        let (x_km, y_km) = ((-460.0, 460.0), (-460.0, 460.0));
        let posts = [65u32, 65];
        let cover = cover_for(site, x_km, y_km, posts, 6, 256).expect("a Colorado box covers");
        let world = 256.0 * 2f64.powi(6);
        for j in 0..posts[1] {
            for i in 0..posts[0] {
                let (lat, lon) = post_geo(site, x_km, y_km, posts, i, j);
                let (px, py) = global_px(lat, lon, 6, 256);
                assert!(
                    px - 0.5 >= f64::from(cover.tx0 * 256)
                        && px + 0.5 <= f64::from((cover.tx1 + 1) * 256)
                        && py - 0.5 >= f64::from(cover.ty0 * 256)
                        && py + 0.5 <= f64::from((cover.ty1 + 1) * 256),
                    "post ({i},{j}) at ({px},{py}) of {world} escapes {cover:?}"
                );
            }
        }
        // Falsifiability floor: the cover is a real rectangle of several tiles,
        // not the whole world and not one tile that trivially contains
        // everything.
        assert!(
            cover.len() > 1 && cover.len() < 64,
            "cover is {} tiles: {cover:?}",
            cover.len()
        );
    }

    #[test]
    fn a_box_wrapping_the_antimeridian_is_refused_rather_than_fetching_the_world() {
        let err = cover_for(
            (0.0, 179.9),
            (-460.0, 460.0),
            (-460.0, 460.0),
            [33, 33],
            6,
            256,
        );
        assert_eq!(err, Err(ElevationError::CrossesAntimeridian));
    }
}
