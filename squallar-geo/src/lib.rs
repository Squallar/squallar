//! The workspace's horizontal-geodesy floor: one sphere, one Web Mercator,
//! and the polygon/bounds vocabulary overlay features are built from.
//!
//! [`EARTH_RADIUS_KM`] and the [`KM_PER_DEGREE_LAT`] derived from it are the
//! only sphere anything above may convert degrees to ground kilometres on.

use std::f64::consts::PI;

/// Mean radius of Earth in kilometers — the IUGG mean radius, and the one
/// sphere every *horizontal* measurement in this workspace stands on.
///
/// The *horizontal* radius only: `squallar-radar`'s `beam::RE_EFF_KM`
/// (`6371 · 4/3`) is a refraction model and the `1.21 · 6371` Level III models
/// reproduce RPG products.
pub const EARTH_RADIUS_KM: f64 = 6371.0;

/// Kilometres per degree of latitude on [`EARTH_RADIUS_KM`]: 111.194927 km.
///
/// Derived, never written down, so no caller can hold a different planet. The
/// only copy that is not this expression is `volume.wgsl`'s, which cannot see
/// Rust. Neither this nor the equatorial 111.32 is "correct" — a real degree
/// runs 110.57 km at the equator to 111.69 km at the poles — so the choice is
/// consistency with the sphere the data is on.
pub const KM_PER_DEGREE_LAT: f64 = EARTH_RADIUS_KM * PI / 180.0;

/// Initial great-circle bearing (degrees clockwise from true north, `0..360`)
/// and surface distance (km) from a radar site to a geographic point.
///
/// Haversine distance on [`EARTH_RADIUS_KM`] and the standard forward azimuth.
///
/// Distance is a *ground* range, so pairing it with a slant-range gate index
/// wants `squallar-radar`'s `beam::slant_range_for_ground_km` in between.
pub fn site_bearing_range_km(site_lat: f64, site_lon: f64, lat: f64, lon: f64) -> (f64, f64) {
    let lat1 = site_lat.to_radians();
    let lon1 = site_lon.to_radians();
    let lat2 = lat.to_radians();
    let lon2 = lon.to_radians();
    let dlat = lat2 - lat1;
    let dlon = lon2 - lon1;

    // Clamped: the haversine can round a hair *over* 1.0 for a near-antipodal pair,
    // and `(1.0 - a).sqrt()` is then `NaN`. Measured: 3.7 % of pairs.
    let a = ((dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2))
        .clamp(0.0, 1.0);
    let range_km = EARTH_RADIUS_KM * 2.0 * a.sqrt().atan2((1.0 - a).sqrt());

    let y = dlon.sin() * lat2.cos();
    let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * dlon.cos();
    let bearing_deg = (y.atan2(x).to_degrees() + 360.0) % 360.0;

    (bearing_deg, range_km)
}

/// Where a point `ground_range_km` from the site along initial bearing
/// `bearing_deg` actually is, as `(lat, lon)` in degrees — the exact inverse of
/// [`site_bearing_range_km`].
///
/// Distance is a **ground** range; a caller holding a slant range applies
/// `beam::ground_range_km` first.
pub fn great_circle_destination(
    site_lat: f64,
    site_lon: f64,
    bearing_deg: f64,
    ground_range_km: f64,
) -> (f64, f64) {
    let (sin_lat1, cos_lat1) = site_lat.to_radians().sin_cos();
    let (sin_az, cos_az) = bearing_deg.to_radians().sin_cos();
    let (sin_d, cos_d) = (ground_range_km / EARTH_RADIUS_KM).sin_cos();

    // Clamped for the same reason the haversine is: the sum can round a hair past
    // ±1 for a range landing on a pole.
    let sin_lat2 = (sin_lat1 * cos_d + cos_lat1 * sin_d * cos_az).clamp(-1.0, 1.0);
    let dlon = (sin_az * sin_d * cos_lat1).atan2(cos_d - sin_lat1 * sin_lat2);

    (sin_lat2.asin().to_degrees(), site_lon + dlon.to_degrees())
}

// Refuse on `hav`, not on `sin d`, with a threshold derived from the
// conditioning. With `u = 1 − hav`, `d = π − 2√u + O(u^1.5)`, so `hav`'s
// last ulp lands on `d` amplified to ≈ ε/√u while the divisor `sin d` is
// only ≈ 2√u; the relative error ≈ ε/(2u) passes 1 % once `u` < ~50ε.
// Testing `d` or `sin d` instead misses the 680 of 3602 antipodal latitude
// pairs whose `hav` is not exactly 1.0. Guard withdraws below 1.519 m.
pub fn great_circle_point(a: (f64, f64), b: (f64, f64), t: f64) -> (f64, f64) {
    let (lat1, lon1) = (a.0.to_radians(), a.1.to_radians());
    let (lat2, lon2) = (b.0.to_radians(), b.1.to_radians());

    let dlat = lat2 - lat1;
    let dlon = lon2 - lon1;
    // Clamped for the reason given in `site_bearing_range_km`.
    let hav = ((dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2))
        .clamp(0.0, 1.0);
    let d = 2.0 * hav.sqrt().atan2((1.0 - hav).sqrt());

    // Refuse on `hav`, not on `sin d`, with a threshold derived from the
    // conditioning. With `u = 1 − hav`, `d = π − 2√u + O(u^1.5)`, so `hav`'s last
    // ulp lands on `d` amplified to ≈ ε/√u while the divisor `sin d` is only ≈ 2√u;
    // the divisor's relative error ≈ ε/(2u) passes 1 % once `u` drops under ~50ε.
    // Over 3602 antipodal latitude pairs only 2922 give `hav` exactly 1.0, so
    // testing `d` or `sin d` instead misses the rest. The guard withdraws below a
    // 1.519 m separation.
    const MIN_CONDITIONING: f64 = 64.0 * f64::EPSILON;
    if hav < MIN_CONDITIONING || 1.0 - hav < MIN_CONDITIONING {
        return a;
    }
    let sin_d = d.sin();

    let ka = ((1.0 - t) * d).sin() / sin_d;
    let kb = (t * d).sin() / sin_d;

    let x = ka * lat1.cos() * lon1.cos() + kb * lat2.cos() * lon2.cos();
    let y = ka * lat1.cos() * lon1.sin() + kb * lat2.cos() * lon2.sin();
    let z = ka * lat1.sin() + kb * lat2.sin();

    (z.atan2(x.hypot(y)).to_degrees(), y.atan2(x).to_degrees())
}

/// A point on the ground, in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeoPoint {
    pub lat: f64,
    pub lon: f64,
}

impl GeoPoint {
    /// Whether this names a point that exists: latitude in `[-90, 90]`,
    /// longitude in `[-180, 180]`.
    ///
    /// Range rather than `is_finite`, and it subsumes it: NaN compares false
    /// against everything and the infinities fall outside the bounds.
    pub fn is_on_earth(self) -> bool {
        (-90.0..=90.0).contains(&self.lat) && (-180.0..=180.0).contains(&self.lon)
    }
}

/// Ring of (latitude, longitude) points. First ring is exterior, rest are holes.
pub type GeoPolygonRing = Vec<(f64, f64)>;

pub type GeoPolygon = Vec<GeoPolygonRing>;

/// Geographic bounding box for viewport culling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoBounds {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
}

impl GeoBounds {
    pub fn intersects(&self, other: &GeoBounds) -> bool {
        self.min_lat <= other.max_lat
            && self.max_lat >= other.min_lat
            && self.min_lon <= other.max_lon
            && self.max_lon >= other.min_lon
    }

    /// Whether `(lat, lon)` is inside the box, **inclusive on all four edges**,
    /// matching [`GeoBounds::intersects`].
    pub fn contains_point(&self, lat: f64, lon: f64) -> bool {
        !(lat < self.min_lat || lat > self.max_lat || lon < self.min_lon || lon > self.max_lon)
    }

    /// The workspace's one min/max bounds fold: the tightest box around
    /// every `(lat, lon)` yielded, `None` when the iterator yields nothing.
    ///
    /// `f64::min`/`f64::max` never adopt a `NaN`, so a `NaN` vertex leaves every
    /// edge where it was.
    pub fn from_points(points: impl IntoIterator<Item = (f64, f64)>) -> Option<GeoBounds> {
        let mut min_lat = f64::MAX;
        let mut max_lat = f64::MIN;
        let mut min_lon = f64::MAX;
        let mut max_lon = f64::MIN;
        let mut any = false;

        for (lat, lon) in points {
            min_lat = min_lat.min(lat);
            max_lat = max_lat.max(lat);
            min_lon = min_lon.min(lon);
            max_lon = max_lon.max(lon);
            any = true;
        }

        if any {
            Some(GeoBounds {
                min_lat,
                max_lat,
                min_lon,
                max_lon,
            })
        } else {
            None
        }
    }
}

/// `None` when there is not a single vertex.
pub fn compute_geo_bounds(polygons: &[GeoPolygon]) -> Option<GeoBounds> {
    GeoBounds::from_points(
        polygons
            .iter()
            .flatten()
            .flat_map(|ring| ring.iter().copied()),
    )
}

/// Where a finished raster belongs on the ground, computed once at delivery.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacedRaster {
    /// The four edges the pixels span.
    pub geo: GeoBounds,
    /// Web Mercator `y` of `geo.min_lat` and `geo.max_lat`, in that order.
    pub mercator_y: (f64, f64),
}

impl PlacedRaster {
    /// The one constructor: the mercator pair is **derived**, never supplied.
    pub fn of(geo: GeoBounds) -> Self {
        Self {
            mercator_y: (
                lat_rad_to_mercator_y(geo.min_lat.to_radians()),
                lat_rad_to_mercator_y(geo.max_lat.to_radians()),
            ),
            geo,
        }
    }
}

/// The latitude Web Mercator ends at: the one whose projected `y` is exactly
/// `π`, so the world is the square the tile grid needs it to be.
///
/// `2·atan(e^π) − π/2` in degrees, to the digits EPSG:3857 and the OSM
/// slippy-map note carry; the truncated `85.05` is 125.51 m of meridian short.
pub const MERCATOR_LAT_LIMIT_DEG: f64 = 85.051_128_779_806_6;

#[inline]
pub fn lat_rad_to_mercator_y(lat_rad: f64) -> f64 {
    (PI / 4.0 + lat_rad / 2.0).tan().ln()
}

/// The latitude Web Mercator ends at: the one whose projected `y` is exactly
/// `π`, so the world is the square the tile grid needs it to be.
///
/// `2·atan(e^π) − π/2` in degrees, to the digits EPSG:3857 and the OSM
/// slippy-map note carry; the truncated `85.05` is 125.51 m of meridian short.
/// going through the angle would cost an `asin` and a `tan` per sample in a
/// ~28 M-sample loop. Both helpers keep `#[inline]` for the same reason.
///
/// `sin φ` of exactly ±1 is a pole: this returns ±∞ there, as the angle form
/// does. Outside ±1 it returns `NaN`.
#[inline]
pub fn mercator_y_from_sin_lat(sin_lat: f64) -> f64 {
    // `0.5 · ln((1 + s)/(1 − s))` rather than `s.atanh()`: identical in exact
    // arithmetic, and this spelling survives `s == 1.0` as `+∞`.
    0.5 * ((1.0 + sin_lat) / (1.0 - sin_lat)).ln()
}

/// The one inverse Web Mercator: the latitude, in **radians**, whose
/// [`lat_rad_to_mercator_y`] is `merc_y` — the Gudermannian, `atan(sinh y)`.
///
/// `atan ∘ sinh` rather than `2·atan(eʸ) − π/2`: the two agree to an ulp, but
/// the doubled-and-shifted form reaches a pole-adjacent latitude by cancelling
/// two quantities the size of `π`.
pub fn mercator_y_to_lat_rad(merc_y: f64) -> f64 {
    merc_y.sinh().atan()
}

// ── Slippy tiles: the same Web Mercator, quantized to `2^zoom × 2^zoom`. ──

/// Carry a fractional tile coordinate to an index on `0..2^zoom`.
///
/// Clamps at **both** ends, matching `mercantile`. The saturating `as` matters
/// on the way in too — −90° through the old `ln(tan φ + sec φ)` gave `u32::MAX`,
/// and a caller's `+ 1` on that is a debug-build overflow panic.
#[inline]
fn tile_index(coord: f64, zoom: u8) -> u32 {
    // NaN floors to NaN and `NaN as u32` is 0, which is the low edge.
    let last = 2u32.saturating_pow(u32::from(zoom)).saturating_sub(1);
    (coord.floor().max(0.0) as u32).min(last)
}

/// Convert longitude to tile X index at the given zoom level.
///
/// Clamped to the grid at both ends. Longitudes outside ±180 are **clamped, not
/// wrapped**: a viewport straddling the antimeridian loses the far side.
pub fn lon_to_tile_x(lon: f64, zoom: u8) -> u32 {
    let n = 2f64.powi(zoom as i32);
    tile_index((lon + 180.0) / 360.0 * n, zoom)
}

/// Convert latitude to tile Y index at the given zoom level.
///
/// `asinh(tan φ)`, not `ln(tan φ + sec φ)`: exactly the same function, but the
/// sum cancels south of the equator — at −89.9999° the old form is 188 px out
/// at zoom 18. `walkers` writes `tan().asinh()`.
pub fn lat_to_tile_y(lat: f64, zoom: u8) -> u32 {
    let n = 2f64.powi(zoom as i32);
    let y = lat.to_radians().tan().asinh();
    tile_index((1.0 - y / std::f64::consts::PI) / 2.0 * n, zoom)
}

/// Convert tile X index back to the western longitude of the tile.
pub fn tile_to_lon(x: u32, zoom: u8) -> f64 {
    let n = 2f64.powi(zoom as i32);
    x as f64 / n * 360.0 - 180.0
}

/// Convert tile Y index back to the northern latitude of the tile.
pub fn tile_to_lat(y: u32, zoom: u8) -> f64 {
    let n = 2f64.powi(zoom as i32);
    mercator_y_to_lat_rad(PI * (1.0 - 2.0 * y as f64 / n)).to_degrees()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bit-level distance between two finite `f64`s, in units in the last place.
    fn ulp_distance(a: f64, b: f64) -> u64 {
        fn ordered(x: f64) -> i64 {
            let bits = x.to_bits() as i64;
            if bits < 0 {
                i64::MIN.wrapping_sub(bits)
            } else {
                bits
            }
        }
        ordered(a).abs_diff(ordered(b))
    }

    /// [`mercator_y_to_lat_rad`] inverts [`lat_rad_to_mercator_y`] to within 4 ulps.
    /// The equator is asserted absolutely because the ulp metric degenerates at zero.
    #[test]
    fn the_inverse_mercator_round_trips_the_forward() {
        for lat_deg in [-60.0_f64, -45.0, 0.0, 45.0, 60.0] {
            let lat_rad = lat_deg.to_radians();
            let back = mercator_y_to_lat_rad(lat_rad_to_mercator_y(lat_rad));
            if lat_rad == 0.0 {
                assert!(
                    back.abs() <= 2.0 * f64::EPSILON,
                    "equator round trip landed {back:e} rad from 0"
                );
            } else {
                let ulps = ulp_distance(lat_rad, back);
                assert!(
                    ulps <= 4,
                    "{lat_deg}° round trip is {ulps} ulps out: {lat_rad:e} -> {back:e}"
                );
            }
        }
    }
}
