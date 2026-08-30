//! WebMercatorQuad grid arithmetic.
//!
//! Every raster chunk is warped onto the exact pixel grid a WebMercatorQuad
//! pyramid uses, or the tiles one chunk contributes do not line up with its
//! neighbour's and the seams show.
//!
//! Degree cells are the right unit for the contour pass, which works in the
//! DEM's own EPSG:4326 grid. They are the WRONG unit for the raster pass:
//! Mercator stretches vertically by sec(lat), so a 5x5 degree cell at 80N is
//! ~5.7x taller in pixels than the same cell at the equator — 14564 x 83000 px
//! at z12, 4.8 TB as Float32 for ONE chunk. Raster chunks therefore count
//! TILES, which is uniform everywhere on the globe by construction.

use std::f64::consts::PI;

use squallar_geo::{lat_to_tile_y, lon_to_tile_x, tile_to_lat, tile_to_lon};

/// EPSG:3857's sphere radius, in metres.
///
/// Not [`squallar_geo::EARTH_RADIUS_KM`], which is the 6371 km mean sphere used
/// for ground ranges. Web Mercator is defined on 6378137 m exactly, and mixing
/// the two puts every tile in the wrong place.
pub const MERCATOR_R: f64 = 6_378_137.0;

/// Half the width of the projected world, in metres: `x` and `y` both run
/// `-ORIGIN ..= ORIGIN`.
pub const ORIGIN: f64 = PI * MERCATOR_R;

/// Edge of one WebMercatorQuad tile, in pixels.
pub const TILE_PX: u32 = 256;

fn tiles_across(zoom: u8) -> f64 {
    2f64.powi(i32::from(zoom))
}

fn last_index(zoom: u8) -> u32 {
    2u32.saturating_pow(u32::from(zoom)).saturating_sub(1)
}

/// Fractional tile column of a longitude.
pub fn frac_tile_x(lon: f64, zoom: u8) -> f64 {
    (lon + 180.0) / 360.0 * tiles_across(zoom)
}

/// Fractional tile row of a latitude.
///
/// `asinh(tan φ)` because that is the spelling [`squallar_geo::lat_to_tile_y`]
/// uses, and a tile this build writes has to carry the address the app asks for.
/// `tests::frac_tile_row_floors_to_the_library_index` is what holds the two
/// together; it reddens if either side changes form.
pub fn frac_tile_y(lat: f64, zoom: u8) -> f64 {
    let merc_y = lat.to_radians().tan().asinh();
    (1.0 - merc_y / PI) / 2.0 * tiles_across(zoom)
}

/// An inclusive tile range, in tile indices at one zoom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileRange {
    pub tx0: u32,
    pub ty0: u32,
    pub tx1: u32,
    pub ty1: u32,
}

impl TileRange {
    /// Whether two ranges share at least one tile.
    ///
    /// Both are INCLUSIVE on all four edges — `tx1`/`ty1` are tiles that belong
    /// to the range, not one-past-the-end — so the comparisons are `<=`. Using
    /// `<` here would drop the super-cells along a region's south and east
    /// borders, which is the half of a clip nobody looks at.
    pub fn intersects(self, other: Self) -> bool {
        self.tx0 <= other.tx1
            && other.tx0 <= self.tx1
            && self.ty0 <= other.ty1
            && other.ty0 <= self.ty1
    }
}

/// A lon/lat rectangle in degrees, as `RASTER_BBOX=west,south,east,north`.
///
/// Distinct from [`Bbox`], which is a whole-degree box DERIVED from a tile range
/// and carries the centre latitude `gdaldem` needs. This one is an input: the
/// region an operator asked for, in the order every GDAL `-te`-adjacent tool
/// spells it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LonLatBox {
    pub w: f64,
    pub s: f64,
    pub e: f64,
    pub n: f64,
}

impl LonLatBox {
    /// The inclusive tile range this box covers at `zoom`.
    ///
    /// [`tile_range`] unchanged, so a clip and a warp read the same arithmetic.
    /// That carries `tile_range`'s edge rule with it: a box edge landing exactly
    /// on a tile boundary does not claim the tile that STARTS there. Regional
    /// bounding boxes are quoted in whole or tenth degrees and never land on a
    /// Mercator tile boundary, so this is a documented property rather than a
    /// live hazard — and a second spelling of the projection would be worse.
    pub fn tile_range(self, zoom: u8) -> TileRange {
        tile_range(zoom, self.w, self.s, self.e, self.n)
    }
}

impl std::fmt::Display for LonLatBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{},{},{},{}", self.w, self.s, self.e, self.n)
    }
}

impl std::str::FromStr for LonLatBox {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.split(',').map(str::trim).collect();
        let [w, s_, e, n] = parts.as_slice() else {
            return Err(format!(
                "expected four comma-separated degrees west,south,east,north; got {} field(s)",
                parts.len()
            ));
        };
        let mut v = [0.0f64; 4];
        for (slot, (field, label)) in
            v.iter_mut()
                .zip([(w, "west"), (s_, "south"), (e, "east"), (n, "north")])
        {
            *slot = field
                .parse::<f64>()
                .ok()
                .filter(|d| d.is_finite())
                .ok_or_else(|| format!("{label}={field:?} is not a finite number of degrees"))?;
        }
        Ok(Self {
            w: v[0],
            s: v[1],
            e: v[2],
            n: v[3],
        })
    }
}

/// A rectangle in EPSG:3857 metres, and the pixel grid covering it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Extent {
    pub xmin: f64,
    pub ymin: f64,
    pub xmax: f64,
    pub ymax: f64,
    pub nx: u32,
    pub ny: u32,
}

/// The lon/lat box a tile range covers, widened to whole degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bbox {
    pub w: i32,
    pub s: i32,
    pub e: i32,
    pub n: i32,
    /// Centre latitude, which `gdaldem hillshade -s` needs as a scalar.
    pub clat: f64,
}

/// The inclusive tile range covering a lon/lat box.
///
/// The low corners are [`lon_to_tile_x`] and [`lat_to_tile_y`] unchanged. The
/// high corners are `ceil(t) - 1`, so a box whose edge falls exactly on a tile
/// boundary does not claim the tile that starts there — `floor` would widen
/// every whole-degree chunk by one tile column and one row, and neighbouring
/// chunks would then both own their shared seam.
///
/// squallar-geo CLAMPS rather than wraps at ±180 and at the Mercator latitude
/// limit, and that is correct here: chunks are aligned to whole degrees from
/// −180, no Copernicus cell straddles the antimeridian, and the one caller that
/// asks for the whole world passes exactly ±180 and wants the grid edge.
pub fn tile_range(zoom: u8, w: f64, s: f64, e: f64, n: f64) -> TileRange {
    let last = last_index(zoom);
    let tx0 = lon_to_tile_x(w, zoom);
    let ty0 = lat_to_tile_y(n, zoom);
    let tx1 = ceil_index(frac_tile_x(e, zoom), last).max(tx0);
    let ty1 = ceil_index(frac_tile_y(s, zoom), last).max(ty0);
    TileRange { tx0, ty0, tx1, ty1 }
}

/// `ceil(t) - 1`, carried onto `0..=last`. NaN floors to the low edge, matching
/// [`squallar_geo`]'s own saturating cast.
fn ceil_index(t: f64, last: u32) -> u32 {
    let v = t.ceil() - 1.0;
    if v.is_nan() || v <= 0.0 {
        0
    } else if v > f64::from(last) {
        last
    } else {
        v as u32
    }
}

/// Column `t` of `2^zoom`, in EPSG:3857 metres.
///
/// The grid's own linear map, not a projection: the projection is entirely in
/// [`frac_tile_x`] / [`frac_tile_y`] and in squallar-geo.
fn merc_x(t: u32, zoom: u8) -> f64 {
    (2.0 * f64::from(t) / tiles_across(zoom) - 1.0) * ORIGIN
}

/// Row `t` of `2^zoom`, in EPSG:3857 metres. Rows count southward from `+ORIGIN`.
fn merc_y(t: u32, zoom: u8) -> f64 {
    (1.0 - 2.0 * f64::from(t) / tiles_across(zoom)) * ORIGIN
}

/// The metre rectangle and pixel grid of a tile range.
pub fn tile_extent(zoom: u8, r: TileRange) -> Extent {
    Extent {
        xmin: merc_x(r.tx0, zoom),
        ymin: merc_y(r.ty1 + 1, zoom),
        xmax: merc_x(r.tx1 + 1, zoom),
        ymax: merc_y(r.ty0, zoom),
        nx: (r.tx1 - r.tx0 + 1) * TILE_PX,
        ny: (r.ty1 - r.ty0 + 1) * TILE_PX,
    }
}

/// The metre rectangle and pixel grid covering a lon/lat box.
pub fn extent(zoom: u8, w: f64, s: f64, e: f64, n: f64) -> Extent {
    tile_extent(zoom, tile_range(zoom, w, s, e, n))
}

/// The whole-degree box a tile range covers, with a one-degree margin.
///
/// The margin is what the source enumeration needs: the resampling kernel and
/// `gdaldem`'s 3x3 window must see real neighbours rather than an edge, or a
/// grid of seams appears at every super-cell border.
pub fn tile_bbox(zoom: u8, r: TileRange) -> Bbox {
    let south = tile_to_lat(r.ty1 + 1, zoom);
    let north = tile_to_lat(r.ty0, zoom);
    Bbox {
        w: tile_to_lon(r.tx0, zoom).floor() as i32 - 1,
        s: south.floor() as i32 - 1,
        e: tile_to_lon(r.tx1 + 1, zoom).ceil() as i32 + 1,
        n: north.ceil() as i32 + 1,
        clat: (south + north) / 2.0,
    }
}

impl Extent {
    /// `xmin ymin xmax ymax nx ny`, the argument order `gdalwarp -te`/`-ts` takes.
    pub fn line(&self) -> String {
        format!(
            "{:.10} {:.10} {:.10} {:.10} {} {}",
            self.xmin, self.ymin, self.xmax, self.ymax, self.nx, self.ny
        )
    }
}

impl Bbox {
    pub fn line(&self) -> String {
        format!(
            "{} {} {} {} {:.6}",
            self.w, self.s, self.e, self.n, self.clat
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `TileRange` is inclusive on all four edges, so two ranges that share
    /// exactly ONE tile intersect. Every corner is checked: a `<` typo on one
    /// axis loses one border of every clipped region and nothing else, which is
    /// invisible until someone pans there.
    #[test]
    fn two_ranges_sharing_one_tile_intersect() {
        let want = TileRange {
            tx0: 312,
            ty0: 694,
            tx1: 648,
            ty1: 883,
        };
        for (label, r) in [
            (
                "south-east corner tile",
                TileRange {
                    tx0: 648,
                    ty0: 883,
                    tx1: 711,
                    ty1: 946,
                },
            ),
            (
                "north-west corner tile",
                TileRange {
                    tx0: 249,
                    ty0: 631,
                    tx1: 312,
                    ty1: 694,
                },
            ),
            (
                "one tile, dead centre",
                TileRange {
                    tx0: 400,
                    ty0: 800,
                    tx1: 400,
                    ty1: 800,
                },
            ),
        ] {
            assert!(r.intersects(want), "{label}");
            assert!(want.intersects(r), "{label}, the other way round");
        }
    }

    /// One tile past each edge is a miss, or the clip is not a clip.
    #[test]
    fn a_range_one_tile_outside_does_not_intersect() {
        let want = TileRange {
            tx0: 312,
            ty0: 694,
            tx1: 648,
            ty1: 883,
        };
        for (label, r) in [
            (
                "east",
                TileRange {
                    tx0: 649,
                    ty0: 694,
                    tx1: 712,
                    ty1: 883,
                },
            ),
            (
                "west",
                TileRange {
                    tx0: 248,
                    ty0: 694,
                    tx1: 311,
                    ty1: 883,
                },
            ),
            (
                "south",
                TileRange {
                    tx0: 312,
                    ty0: 884,
                    tx1: 648,
                    ty1: 947,
                },
            ),
            (
                "north",
                TileRange {
                    tx0: 312,
                    ty0: 630,
                    tx1: 648,
                    ty1: 693,
                },
            ),
        ] {
            assert!(!r.intersects(want), "{label}");
            assert!(!want.intersects(r), "{label}, the other way round");
        }
    }
    // Only the tests pin the Mercator limit; the module itself works in tile
    // coordinates and never names it.
    use squallar_geo::MERCATOR_LAT_LIMIT_DEG;

    /// The tie between this module's fractional row and the library's integer
    /// one. If squallar-geo re-spells its projection, or this module drifts to
    /// `ln(tan(π/4 + φ/2))`, the two stop agreeing on some boundary latitude
    /// and this reddens.
    #[test]
    fn frac_tile_row_floors_to_the_library_index() {
        for zoom in [0u8, 1, 8, 12, 14, 18] {
            let last = f64::from(last_index(zoom));
            for step in 0..=340 {
                let lat = -85.0 + f64::from(step) * 0.5;
                let want = lat_to_tile_y(lat, zoom);
                let got = frac_tile_y(lat, zoom).floor().clamp(0.0, last) as u32;
                assert_eq!(want, got, "lat {lat} at z{zoom}");
            }
        }
    }

    #[test]
    fn frac_tile_column_floors_to_the_library_index() {
        for zoom in [0u8, 1, 8, 12, 14, 18] {
            let last = f64::from(last_index(zoom));
            for step in 0..=720 {
                let lon = -180.0 + f64::from(step) * 0.5;
                let want = lon_to_tile_x(lon, zoom);
                let got = frac_tile_x(lon, zoom).floor().clamp(0.0, last) as u32;
                assert_eq!(want, got, "lon {lon} at z{zoom}");
            }
        }
    }

    /// A whole-degree box must not claim the tile that begins exactly on its
    /// east or south edge; `floor` on the high corner would, and neighbouring
    /// chunks would then both own the seam.
    #[test]
    fn a_box_ending_on_a_tile_boundary_stops_one_tile_short() {
        // At z1 the world is 2x2 tiles; lon 0 is exactly the tx=1 boundary.
        let r = tile_range(1, -180.0, 0.0, 0.0, MERCATOR_LAT_LIMIT_DEG);
        assert_eq!(
            r,
            TileRange {
                tx0: 0,
                ty0: 0,
                tx1: 0,
                ty1: 0
            }
        );
        assert_eq!(
            lon_to_tile_x(0.0, 1),
            1,
            "the library floor would claim tx=1"
        );
    }

    /// A degenerate box still yields one tile rather than an empty range.
    #[test]
    fn a_zero_width_box_still_covers_one_tile() {
        let r = tile_range(12, -105.5, 39.5, -105.5, 39.5);
        assert_eq!(r.tx0, r.tx1);
        assert_eq!(r.ty0, r.ty1);
    }

    /// The whole world at z8 is the 65536x65536 grid the global elevation
    /// raster is built on — 8.6 GB as Float32, the largest single raster this
    /// build ever materialises.
    #[test]
    fn the_global_z8_grid_is_square_and_whole() {
        let e = extent(
            8,
            -180.0,
            -MERCATOR_LAT_LIMIT_DEG,
            180.0,
            MERCATOR_LAT_LIMIT_DEG,
        );
        assert_eq!((e.nx, e.ny), (65536, 65536));
        assert!((e.xmin + ORIGIN).abs() < 1e-6, "{}", e.xmin);
        assert!((e.xmax - ORIGIN).abs() < 1e-6, "{}", e.xmax);
    }

    /// The extent's corners are the tile grid's, so two ranges that abut in
    /// tile space abut exactly in metres — that is what keeps the seams closed.
    #[test]
    fn abutting_tile_ranges_share_an_edge_exactly() {
        let a = tile_extent(
            12,
            TileRange {
                tx0: 100,
                ty0: 200,
                tx1: 163,
                ty1: 263,
            },
        );
        let b = tile_extent(
            12,
            TileRange {
                tx0: 164,
                ty0: 200,
                tx1: 227,
                ty1: 263,
            },
        );
        assert_eq!(a.xmax, b.xmin);
        assert_eq!(a.ymin, b.ymin);
    }

    #[test]
    fn the_bbox_of_a_range_brackets_it_with_a_margin() {
        let r = tile_range(12, -106.0, 39.0, -105.0, 40.0);
        let b = tile_bbox(12, r);
        assert!(b.w <= -106 && b.e >= -105, "{b:?}");
        assert!(b.s <= 39 && b.n >= 40, "{b:?}");
        assert!(b.clat > 39.0 && b.clat < 40.5, "{b:?}");
    }
}
