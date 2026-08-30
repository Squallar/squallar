//! One tile address, and the one projection out of it.
//!
//! A vector-tile feature arrives in **extent units**: integers on a grid whose
//! side the layer declares (4096 in every OpenMapTiles build, and in the
//! archive this workspace ships), with `y` increasing *southward* because the
//! grid is a picture. Getting a prism onto the ground means carrying that
//! through Web Mercator to a geographic point and then into the volume box's
//! kilometres.
//!
//! **Both halves are `squallar-geo`'s own, and that is the whole design.**
//! `squallar_elevation::resample::post_geo` builds the height field's posts by
//! going box kilometres -> [`squallar_geo::great_circle_destination`] ->
//! geographic, and `volume.wgsl` inverts the same map to drape the basemap
//! over the result. This module goes the other way through
//! [`squallar_geo::site_bearing_range_km`], which is that function's documented
//! exact inverse, and through [`squallar_geo::mercator_y_to_lat_rad`], which is
//! the workspace's one inverse Web Mercator. Nothing is re-derived here, so
//! there is no second projection for a building to disagree with the ground
//! under it about.

use std::f64::consts::PI;

/// The deepest tile zoom this crate will read.
///
/// **A refusal ceiling, not a budget.** `2^30` tiles on a side already exceeds
/// what any archive publishes by a wide margin, and the number exists so that
/// `1u32 << zoom` in [`TileId::is_addressable`] cannot shift past the width of
/// the counter on a payload that arrived off the wire.
pub const MAX_TILE_ZOOM: u8 = 30;

/// A slippy-tile address: the same `(z, x, y)` the archive is keyed by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileId {
    /// Whether this address names a tile that can exist: a zoom this crate
    /// reads, and a column and row inside that zoom's grid.
    pub fn is_addressable(&self) -> bool {
        if self.z > MAX_TILE_ZOOM {
            return false;
        }
        let side = 1u32 << self.z;
        self.x < side && self.y < side
    }

    /// Where an extent-unit point inside this tile is, as `(latitude,
    /// longitude)` in degrees.
    ///
    /// `extent` is the layer's own declared extent rather than a constant:
    /// a tile is free to publish any, and reading 4096 into a layer that
    /// declared something else puts every building in it somewhere off the
    /// side of the tile.
    ///
    /// **`y` is flipped here and nowhere else.** Extent units run southward
    /// and everything downstream of this function runs northward.
    pub fn point_geo(&self, extent: u32, px: f64, py: f64) -> (f64, f64) {
        // `powi` and not a shift: this is reachable with any `z` a decoder let
        // through, and `2f64.powi` is total where `1 << z` is a panic in debug
        // and a wrap in release. It is also the spelling `squallar_geo`'s own
        // `tile_to_lat` uses, so the two cannot drift.
        let n = 2f64.powi(i32::from(self.z));
        let extent = f64::from(extent);
        let gx = (f64::from(self.x) + px / extent) / n;
        let gy = (f64::from(self.y) + py / extent) / n;
        let lon = gx * 360.0 - 180.0;
        let lat = squallar_geo::mercator_y_to_lat_rad(PI * (1.0 - 2.0 * gy)).to_degrees();
        (lat, lon)
    }
}

/// The volume box a mesh is built into: an origin and an axis-aligned
/// rectangle of kilometres about it.
///
/// The same three terms `squallar_elevation::HeightField` carries, and they
/// mean the same thing: `x` is east and `y` is north, both signed kilometres
/// from `site`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxFrame {
    /// `(latitude, longitude)` of the box's origin, degrees.
    pub site: (f64, f64),
    /// East extent as `(low, high)` kilometres about the site.
    pub x_km: (f64, f64),
    /// North extent as `(low, high)` kilometres about the site.
    pub y_km: (f64, f64),
}

impl BoxFrame {
    /// Every term finite and both extents non-empty.
    pub fn is_drawable(&self) -> bool {
        [
            self.site.0,
            self.site.1,
            self.x_km.0,
            self.x_km.1,
            self.y_km.0,
            self.y_km.1,
        ]
        .iter()
        .all(|term| term.is_finite())
            && self.x_km.1 > self.x_km.0
            && self.y_km.1 > self.y_km.0
    }

    /// A geographic point as box kilometres, east then north.
    ///
    /// The inverse of `squallar_elevation::resample::post_geo`, term for term:
    /// that one reads `bearing = atan2(east, north)` and `range = hypot`, and
    /// this one undoes exactly those two.
    pub fn geo_to_km(&self, lat: f64, lon: f64) -> [f64; 2] {
        let (bearing_deg, range_km) =
            squallar_geo::site_bearing_range_km(self.site.0, self.site.1, lat, lon);
        let (sin_b, cos_b) = bearing_deg.to_radians().sin_cos();
        [range_km * sin_b, range_km * cos_b]
    }

    /// Whether an axis-aligned bounding box in box kilometres has any part
    /// inside this frame.
    ///
    /// **Deliberately the permissive arm.** A building straddling the edge of
    /// the drawn footprint is kept whole rather than clipped: clipping a prism
    /// means re-tessellating it against the box, and a building that pokes out
    /// of the far edge of the ground is invisible anyway because there is no
    /// ground under it to stand on.
    pub fn overlaps(&self, bbox: [f64; 4]) -> bool {
        let [x0, y0, x1, y1] = bbox;
        x1 >= self.x_km.0 && x0 <= self.x_km.1 && y1 >= self.y_km.0 && y0 <= self.y_km.1
    }
}

#[cfg(test)]
mod tests;
