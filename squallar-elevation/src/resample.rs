//! Terrain-RGB tiles in, one [`HeightField`] on the volume box's post grid out.
//!
//! Three things here are load-bearing and each has cost a project somewhere.
//!
//! **1. [`squallar_geo::great_circle_destination`] is called directly.** It is
//! the *forward* map — box kilometres to a place on the ground — and it is the
//! same function the volume box is built with. No inverse is written here, so
//! there is no second projection to disagree with the first. The obvious
//! shortcut, a flat "degrees per kilometre" map about the site, is **30.797 km
//! out at the true corner (±460, ±460) of a default 920 km box over Colorado**
//! (computed at 39°N/106°W, this work unit). `tests/resample_oracle.rs` asserts
//! the gap rather than describing it — at **30.32 km**, because the outermost
//! post sits half a cell inside the box edge and there is no post at the
//! corner. Two denominators, and they are never quoted as one number.
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
///
/// `PartialEq` but not `Eq`: [`ElevationError::PastMercatorLimit`] carries the
/// latitude it refused, and an `f64` has no total equality.
#[derive(Clone, Debug, PartialEq)]
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
    /// A tile address was supplied twice.
    ///
    /// Refused rather than resolved: the job that will feed this assembles from
    /// network results, and "which of the two bodies is the real tile" is not a
    /// question this crate can answer. Taking the first would be a silent
    /// choice.
    DuplicateTile { x: u32, y: u32 },
    /// The site or one of the box's kilometre extents was not a finite number.
    ///
    /// Refused at the top rather than allowed to flow through: `f64::min` and
    /// `f64::max` ignore `NaN`, so a `NaN` extent leaves the bounding fold's
    /// seeds untouched and produces an *inverted* rectangle that reads as a
    /// perfectly ordinary cover.
    NonFiniteExtent,
    /// A post fell outside Web Mercator's latitude limit, where the projection
    /// has no pixel to name.
    PastMercatorLimit { lat_deg: f64 },
    /// The plane does not cover the box it was asked to resample.
    ///
    /// The whole failure this exists to prevent: without it the sampler's edge
    /// clamp stands in for tiles nobody fetched, and a height field of clamped
    /// nonsense comes back as `Ok`.
    PlaneDoesNotCoverBox { needed: TileCover, have: TileCover },
    /// A sampled height was not a finite number.
    NonFiniteSample { i: u32, j: u32 },
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
            Self::DuplicateTile { x, y } => write!(f, "tile ({x}, {y}) was supplied twice"),
            Self::NonFiniteExtent => {
                write!(f, "the site or a box extent is not a finite number")
            }
            Self::PastMercatorLimit { lat_deg } => write!(
                f,
                "a post reaches {lat_deg}°, past Web Mercator's {}° limit",
                squallar_geo::MERCATOR_LAT_LIMIT_DEG
            ),
            Self::PlaneDoesNotCoverBox { needed, have } => write!(
                f,
                "the box needs tiles x {}..={} y {}..={} at z{}, and the plane holds \
                 x {}..={} y {}..={} at z{}",
                needed.tx0,
                needed.tx1,
                needed.ty0,
                needed.ty1,
                needed.zoom,
                have.tx0,
                have.tx1,
                have.ty0,
                have.ty1,
                have.zoom
            ),
            Self::NonFiniteSample { i, j } => {
                write!(f, "post ({i}, {j}) sampled to a value that is not finite")
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
    /// Tiles across, or **0** for an inverted rectangle.
    ///
    /// Saturating, and that is not defensive style. The fields are public, so a
    /// caller can spell `tx1 < tx0`; `tx1 - tx0 + 1` then panics with
    /// `attempt to subtract with overflow` in debug and wraps to about four
    /// billion in release, and the wrap is the dangerous half — `len()` would
    /// answer an enormous number for a rectangle holding nothing.
    pub fn tiles_x(&self) -> u32 {
        if self.tx1 < self.tx0 {
            0
        } else {
            self.tx1 - self.tx0 + 1
        }
    }

    /// Tiles down, or 0 for an inverted rectangle. See [`TileCover::tiles_x`].
    pub fn tiles_y(&self) -> u32 {
        if self.ty1 < self.ty0 {
            0
        } else {
            self.ty1 - self.ty0 + 1
        }
    }

    /// Tiles in the whole rectangle.
    pub fn len(&self) -> usize {
        self.tiles_x() as usize * self.tiles_y() as usize
    }

    /// Whether the rectangle names no tiles.
    ///
    /// True exactly when it is inverted on either axis. [`cover_for`] cannot
    /// produce such a cover — it refuses non-finite input, which was the only
    /// way to reach one — but the fields are public and a hand-built cover can
    /// be anything, so this answers rather than panics.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether `(x, y)` is one of the tiles this cover names.
    pub fn contains(&self, x: u32, y: u32) -> bool {
        (self.tx0..=self.tx1).contains(&x) && (self.ty0..=self.ty1).contains(&y)
    }

    /// Whether every tile `other` names is also named here, at the same zoom
    /// and tile size.
    ///
    /// The predicate [`TilePlane::resample`] enforces, so a plane can never
    /// stand in for tiles nobody fetched.
    pub fn covers(&self, other: &TileCover) -> bool {
        self.zoom == other.zoom
            && self.tile_px == other.tile_px
            && !other.is_empty()
            && !self.is_empty()
            && self.tx0 <= other.tx0
            && self.tx1 >= other.tx1
            && self.ty0 <= other.ty0
            && self.ty1 >= other.ty1
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
/// half a cell inside the edges — post 0 of a 2-post axis over `(-10, 10)` is
/// at `-5`, not at `-10`.
///
/// **This is the only definition.** [`HeightField::post_center_km`] delegates
/// here rather than restating the arithmetic; it used to be written twice, and
/// two copies of a half-cell offset can diverge by half a cell — 3.6 km at 129
/// posts over a 920 km box — which is exactly the registration the ground mesh
/// depends on. The `+ 0.5` itself is pinned by
/// `the_posts_are_cell_centres_and_not_cell_edges`, which is written against
/// hand-computed positions rather than against this function.
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
/// **The extremes of latitude and longitude over a great-circle box are on its
/// boundary but not at its corners**, so the whole boundary is walked and not
/// the four corners. A box's greatest latitude is at the *centre* of its north
/// edge, and skipping to the corners loses a whole row of tiles along that edge
/// — 1056 tiles against 1023 for a 920 km box at z10, and the difference is
/// exactly the band the sampler would then answer from its edge clamp.
/// `the_boundary_walk_finds_tiles_the_four_corners_miss` pins that.
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
    // Before anything folds. `f64::min`/`f64::max` never adopt a `NaN`, so a
    // non-finite input leaves `lo`/`hi` at their `f64::MAX`/`f64::MIN` seeds,
    // the antimeridian guard reads `-inf > 180.0` as false, and the clamp turns
    // the untouched seeds into an inverted rectangle that looks like an
    // ordinary cover. Refusing here is the only place it is cheap.
    if ![site.0, site.1, x_km.0, x_km.1, y_km.0, y_km.1]
        .iter()
        .all(|v| v.is_finite())
    {
        return Err(ElevationError::NonFiniteExtent);
    }
    let world = f64::from(tile_px) * 2f64.powi(i32::from(zoom));
    let last = 2u32.saturating_pow(u32::from(zoom)).saturating_sub(1);

    let mut lo = (f64::MAX, f64::MAX);
    let mut hi = (f64::MIN, f64::MIN);
    let mut lon_lo = f64::MAX;
    let mut lon_hi = f64::MIN;
    let mut lat_lo = f64::MAX;
    let mut lat_hi = f64::MIN;
    let mut visit = |i: u32, j: u32| {
        let (lat, lon) = post_geo(site, x_km, y_km, posts, i, j);
        lon_lo = lon_lo.min(lon);
        lon_hi = lon_hi.max(lon);
        lat_lo = lat_lo.min(lat);
        lat_hi = lat_hi.max(lat);
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
    //
    // The out-of-range half is the half that fires, and it only works because
    // that non-normalisation is a contract — see `great_circle_destination`'s
    // own docs, where it is written down and pinned.
    if lon_lo < -180.0 || lon_hi > 180.0 || lon_hi - lon_lo > 180.0 {
        return Err(ElevationError::CrossesAntimeridian);
    }

    // Past Web Mercator's limit there is no pixel to name: `global_px` runs off
    // the top or bottom of the world and the sampler's clamp answers the edge
    // row for every post beyond it — a whole band of posts silently reading one
    // row of pixels. Not reachable from any NEXRAD (the northernmost site is
    // about 65°N, whose box top is 69°N), so this closes a hole in a public API
    // rather than a live defect.
    if lat_hi > squallar_geo::MERCATOR_LAT_LIMIT_DEG {
        return Err(ElevationError::PastMercatorLimit { lat_deg: lat_hi });
    }
    if lat_lo < -squallar_geo::MERCATOR_LAT_LIMIT_DEG {
        return Err(ElevationError::PastMercatorLimit { lat_deg: lat_lo });
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
        for (n, (x, y, _)) in tiles.iter().enumerate() {
            if !cover.contains(*x, *y) {
                return Err(ElevationError::UnexpectedTile { x: *x, y: *y });
            }
            if tiles[..n].iter().any(|(px, py, _)| px == x && py == y) {
                return Err(ElevationError::DuplicateTile { x: *x, y: *y });
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
    /// It is *not* a per-tile clamp; that is the seam this module's point 2
    /// exists to avoid.
    ///
    /// **The clamp will happily answer for a point nowhere near this plane**,
    /// and this method does not stop it — it is the primitive, and a caller
    /// asking for one point may legitimately be probing an edge. What makes the
    /// clamp harmless in the resample is that
    /// [`TilePlane::resample`] refuses a box the plane does not cover, which is
    /// an enforced precondition and not, as this doc used to say, a property
    /// that holds "because `cover_for` takes a one-pixel margin" — nothing had
    /// obliged the caller to have called `cover_for` at all.
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
    ///
    /// **The plane must cover the box, and that is checked here rather than
    /// assumed.** Nothing in the types ties a `TilePlane` to a box:
    /// [`cover_for`] is a free function the caller may have called with
    /// different arguments, or not at all. Without this check a plane over a
    /// 1040 km box resampled onto a 9200 km one returns `Ok`, with a corner
    /// post reading −24.5 m where the truth is −7086.7 m — every post outside
    /// the plane answered by the sampler's edge clamp, and the whole field
    /// plausible. That is the silent-partial-success shape this workspace has
    /// been bitten by before, and it sits exactly where the job layer will
    /// compose: a fetch round that lands a shrunken tile set would otherwise
    /// produce a believable height field and report success.
    ///
    /// The needed cover is recomputed from the box **at this plane's own
    /// declared zoom and tile size**, so a plane whose addresses do not match
    /// the box at the zoom it claims is refused.
    ///
    /// **That is narrower than "a plane at the wrong zoom is refused", which is
    /// what this sentence used to say and is false.** A *mislabelled* zoom is
    /// undetectable here and produces a plausible wrong field: declare `zoom: 8`
    /// for tiles that are really z10, fill every address the honest z8 cover
    /// names with a real body, and this returns `Ok` with heights off by
    /// hundreds of metres (measured: 2735.5-2780.25 m against a truth of
    /// 2367.25-2546.25 m). There is no defence available at this layer -- a PNG
    /// carries no zoom, and every internal consistency check passes because the
    /// declaration is consistent with itself. The invariant "these bodies came
    /// from the zoom they are labelled with" belongs to whatever fetches them,
    /// and it is owed by the first fetch layer to be written.
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
        let needed = cover_for(site, x_km, y_km, posts, self.cover.zoom, self.cover.tile_px)?;
        if !self.cover.covers(&needed) {
            return Err(ElevationError::PlaneDoesNotCoverBox {
                needed,
                have: self.cover,
            });
        }

        let mut samples = Vec::with_capacity(posts[0] as usize * posts[1] as usize);
        for j in 0..posts[1] {
            for i in 0..posts[0] {
                let (lat, lon) = post_geo(site, x_km, y_km, posts, i, j);
                let h = self.sample_height_m(lat, lon);
                // Unreachable through `assemble`, which fills every pixel of
                // every tile it names and refuses a cover with a hole in it.
                // Checked anyway, and this is the asymmetry worth naming: the
                // `u16` encoding has no spare code for absence, so a `NaN`
                // would quietly become a −500 m pit — the very confusion
                // `min_elevation` spends a whole `i16` code to avoid.
                if !h.is_finite() {
                    return Err(ElevationError::NonFiniteSample { i, j });
                }
                samples.push(encode_height_m(h));
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
    ///
    /// **Swept across zooms, and the sweep is the point.** The first version of
    /// this test pinned z6 alone, which is the one zoom where the property is
    /// degenerate: a 920 km box at 39°N needs 9 tiles at z6 whether the cover
    /// walks the whole boundary or only the four corners, so a corners-only
    /// mutant survived it — while the shipped fixture is z10, where the two
    /// answers are 1056 tiles against 1023.
    #[test]
    fn the_cover_holds_every_post_and_its_bilinear_neighbourhood() {
        let site = (39.0, -106.0);
        let (x_km, y_km) = ((-460.0, 460.0), (-460.0, 460.0));
        let posts = [65u32, 65];
        for zoom in [6u8, 8, 10, 12] {
            let tile = 256u32;
            let cover =
                cover_for(site, x_km, y_km, posts, zoom, tile).expect("a Colorado box covers");
            for j in 0..posts[1] {
                for i in 0..posts[0] {
                    let (lat, lon) = post_geo(site, x_km, y_km, posts, i, j);
                    let (px, py) = global_px(lat, lon, zoom, tile);
                    assert!(
                        px - 0.5 >= f64::from(cover.tx0 * tile)
                            && px + 0.5 <= f64::from((cover.tx1 + 1) * tile)
                            && py - 0.5 >= f64::from(cover.ty0 * tile)
                            && py + 0.5 <= f64::from((cover.ty1 + 1) * tile),
                        "z{zoom} post ({i},{j}) at ({px},{py}) escapes {cover:?}"
                    );
                }
            }
            // Falsifiability floor: a real rectangle of several tiles, not the
            // whole world and not one tile that trivially contains everything.
            assert!(
                cover.len() > 1 && cover.len() < 1 << 16,
                "z{zoom} cover is {} tiles: {cover:?}",
                cover.len()
            );
        }
    }

    /// The whole boundary is walked because a great-circle box's extremes are
    /// **not** at its corners: the greatest latitude is at the centre of the
    /// north edge.
    ///
    /// A corners-only cover therefore loses a band of tiles along that edge,
    /// and every post in it would be answered by the sampler's edge clamp. z6
    /// is included to record that it CANNOT see this — the two covers are
    /// identical there — which is why the test above sweeps.
    #[test]
    fn the_boundary_walk_finds_tiles_the_four_corners_miss() {
        let site = (39.0, -106.0);
        let (x_km, y_km) = ((-460.0, 460.0), (-460.0, 460.0));
        let posts = [129u32, 129];

        // The geometry itself, stated before any cover is built.
        let (top_centre_lat, _) = post_geo(site, x_km, y_km, posts, posts[0] / 2, posts[1] - 1);
        let (top_left_lat, _) = post_geo(site, x_km, y_km, posts, 0, posts[1] - 1);
        let (top_right_lat, _) = post_geo(site, x_km, y_km, posts, posts[0] - 1, posts[1] - 1);
        assert!(
            top_centre_lat > top_left_lat && top_centre_lat > top_right_lat,
            "the north edge's centre ({top_centre_lat}) is not above its corners \
             ({top_left_lat}, {top_right_lat}); this test's premise is wrong"
        );

        for (zoom, differs) in [(6u8, false), (8, true), (10, true), (12, true)] {
            let full = cover_for(site, x_km, y_km, posts, zoom, 256).expect("covers");
            let corners = corners_only_cover(site, x_km, y_km, posts, zoom, 256);
            if differs {
                assert!(
                    full.ty0 < corners.ty0,
                    "z{zoom}: the boundary walk reached ty0 {} and the corners {}, \
                     so a corners-only cover would lose nothing",
                    full.ty0,
                    corners.ty0
                );
                assert!(
                    full.len() > corners.len(),
                    "z{zoom}: {} tiles against {}",
                    full.len(),
                    corners.len()
                );
            } else {
                assert_eq!(
                    full.len(),
                    corners.len(),
                    "z{zoom} is recorded as the zoom where the two agree; it no longer does"
                );
            }
        }
    }

    /// The mutant `the_boundary_walk_finds_tiles_the_four_corners_miss` exists
    /// to kill: a cover built from the four corner posts alone.
    fn corners_only_cover(
        site: (f64, f64),
        x_km: (f64, f64),
        y_km: (f64, f64),
        posts: [u32; 2],
        zoom: u8,
        tile_px: u32,
    ) -> TileCover {
        let world = f64::from(tile_px) * 2f64.powi(i32::from(zoom));
        let mut lo = (f64::MAX, f64::MAX);
        let mut hi = (f64::MIN, f64::MIN);
        for (i, j) in [
            (0, 0),
            (posts[0] - 1, 0),
            (0, posts[1] - 1),
            (posts[0] - 1, posts[1] - 1),
        ] {
            let (lat, lon) = post_geo(site, x_km, y_km, posts, i, j);
            let (px, py) = global_px(lat, lon, zoom, tile_px);
            lo = (lo.0.min(px), lo.1.min(py));
            hi = (hi.0.max(px), hi.1.max(py));
        }
        let clamp_px = |v: f64| v.clamp(0.0, world - 1.0);
        TileCover {
            zoom,
            tile_px,
            tx0: (clamp_px(lo.0 - 1.0) / f64::from(tile_px)).floor() as u32,
            ty0: (clamp_px(lo.1 - 1.0) / f64::from(tile_px)).floor() as u32,
            tx1: (clamp_px(hi.0 + 1.0) / f64::from(tile_px)).floor() as u32,
            ty1: (clamp_px(hi.1 + 1.0) / f64::from(tile_px)).floor() as u32,
        }
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

        // Control: the same box eight degrees west does not straddle and is
        // accepted, so the refusal is about the meridian and not the latitude.
        assert!(
            cover_for(
                (0.0, 171.0),
                (-460.0, 460.0),
                (-460.0, 460.0),
                [33, 33],
                6,
                256
            )
            .is_ok()
        );
    }

    /// A non-finite input used to produce `Ok` with an INVERTED rectangle,
    /// because `f64::min`/`max` never adopt a `NaN` and the seeds survived the
    /// fold untouched. `TileCover::len()` then panicked.
    #[test]
    fn a_non_finite_site_or_extent_is_refused_before_anything_folds() {
        let ok = (-460.0, 460.0);
        for (site, x_km, y_km) in [
            ((f64::NAN, -106.0), ok, ok),
            ((39.0, f64::NAN), ok, ok),
            ((39.0, -106.0), (f64::NAN, 460.0), ok),
            ((39.0, -106.0), ok, (-460.0, f64::NAN)),
            ((f64::INFINITY, -106.0), ok, ok),
            ((39.0, f64::NEG_INFINITY), ok, ok),
        ] {
            assert_eq!(
                cover_for(site, x_km, y_km, [33, 33], 6, 256),
                Err(ElevationError::NonFiniteExtent),
                "site {site:?} x {x_km:?} y {y_km:?}"
            );
        }
        // Control: the same shape with every value finite is accepted.
        assert!(cover_for((39.0, -106.0), ok, ok, [33, 33], 6, 256).is_ok());
    }

    /// An inverted rectangle answers "no tiles" instead of panicking or
    /// wrapping to four billion. The fields are public, so one can be built.
    #[test]
    fn an_inverted_cover_reads_as_empty_rather_than_panicking() {
        let inverted = TileCover {
            zoom: 6,
            tile_px: 256,
            tx0: 63,
            ty0: 31,
            tx1: 0,
            ty1: 32,
        };
        assert_eq!(inverted.tiles_x(), 0);
        assert_eq!(inverted.len(), 0);
        assert!(inverted.is_empty());
        assert_eq!(inverted.addresses().count(), 0);
        assert!(!inverted.contains(10, 31));
        assert_eq!(
            TilePlane::assemble(inverted, &[]).unwrap_err(),
            ElevationError::Empty
        );
        // Control: a well-formed cover of the same size is not empty.
        let ok = TileCover {
            tx0: 0,
            tx1: 63,
            ..inverted
        };
        assert_eq!(ok.tiles_x(), 64);
        assert!(!ok.is_empty());
    }

    /// Past Web Mercator's limit there is no pixel to name, so the cover
    /// refuses rather than letting the sampler's clamp answer a whole band of
    /// posts from one row.
    #[test]
    fn a_box_reaching_past_the_mercator_limit_is_refused() {
        let half = (-460.0, 460.0);
        for site_lat in [82.0_f64, 84.0, -84.0] {
            match cover_for((site_lat, -106.0), half, half, [33, 33], 6, 256) {
                Err(ElevationError::PastMercatorLimit { lat_deg }) => assert!(
                    lat_deg.abs() > squallar_geo::MERCATOR_LAT_LIMIT_DEG,
                    "refused at {lat_deg}, which is inside the limit"
                ),
                other => panic!("site at {site_lat} gave {other:?}"),
            }
        }
        // Control: the northernmost NEXRAD is about 65°N, whose box top is
        // ~69°N, and that is accepted. This guard closes a hole in a public
        // API; it does not narrow the sites the app serves.
        let cover = cover_for((65.0, -147.0), half, half, [33, 33], 6, 256)
            .expect("an Alaskan site is well inside the limit");
        let (top_lat, _) = post_geo((65.0, -147.0), half, half, [33, 33], 16, 32);
        assert!(
            (68.0..70.0).contains(&top_lat),
            "the box top is {top_lat}, not the ~69° this control claims"
        );
        assert!(!cover.is_empty());
    }

    /// `covers` is the predicate the resample enforces, and it is strict about
    /// zoom and tile size as well as about the rectangle.
    #[test]
    fn a_cover_only_covers_a_rectangle_it_wholly_contains_at_the_same_zoom() {
        let outer = TileCover {
            zoom: 10,
            tile_px: 256,
            tx0: 10,
            ty0: 20,
            tx1: 20,
            ty1: 30,
        };
        assert!(outer.covers(&outer));
        assert!(outer.covers(&TileCover {
            tx0: 12,
            tx1: 15,
            ty0: 22,
            ty1: 25,
            ..outer
        }));
        // One tile past each edge, in turn.
        assert!(!outer.covers(&TileCover { tx0: 9, ..outer }));
        assert!(!outer.covers(&TileCover { tx1: 21, ..outer }));
        assert!(!outer.covers(&TileCover { ty0: 19, ..outer }));
        assert!(!outer.covers(&TileCover { ty1: 31, ..outer }));
        // A different zoom or tile size is a different grid entirely.
        assert!(!outer.covers(&TileCover { zoom: 11, ..outer }));
        assert!(!outer.covers(&TileCover {
            tile_px: 512,
            ..outer
        }));
        // An empty rectangle is covered by nothing, in either position.
        let empty = TileCover { tx1: 9, ..outer };
        assert!(!outer.covers(&empty));
        assert!(!empty.covers(&outer));
    }
}
