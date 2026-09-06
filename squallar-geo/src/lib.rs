//! The workspace's horizontal-geodesy floor: one sphere, one Web Mercator,
//! and the polygon/bounds vocabulary overlay features are built from.
//!
//! [`EARTH_RADIUS_KM`] and the [`KM_PER_DEGREE_LAT`] derived from it are the
//! only sphere anything above may convert degrees to ground kilometres on.
//!
//! [`min_elevation`] is the one piece of *data* here rather than arithmetic: a
//! global 1°×1° minimum-elevation grid. It sits at this level because the crate
//! that emits it stands above the crate that reads it, and its module docs give
//! the cycle in full.

/// Where the sun is over a point on the ground, and what colour its light is.
///
/// It lives here rather than beside the renderer for the reason every other
/// function in this crate does: it is arithmetic over `std` with no graphics,
/// no clock and no I/O in it, and both the ground pass and the volume have to
/// reach the *same* answer. This crate is band 0 — every rendering crate
/// already stands on it, so nothing above can close a cycle by asking.
pub mod solar;

use std::f64::consts::PI;

pub mod min_elevation;

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
///
/// # The returned longitude is deliberately NOT normalised
///
/// That is a contract, not an accident.
///
/// This returns `site_lon + Δlon`, so a destination past the antimeridian comes
/// back as 184.03° rather than as −175.97°. It is **not** an oversight and it
/// must not be "fixed": callers decide, because they do not want the same thing
/// and one of them detects the wrap by exactly this means.
/// [`the_destination_longitude_is_never_normalised`] pins it.
///
/// The three policies in this workspace, so a fourth caller picks deliberately:
///
/// * `squallar_elevation::resample::cover_for` **depends on the raw value** —
///   an out-of-range longitude is how it tells a box straddling the
///   antimeridian from one that does not, and normalising here would make that
///   guard silently stop firing while its test still passed.
/// * `squallar_source::volume::VolumeGrid::footprint` also keeps the raw
///   value, for a different reason: `GeoBounds` is a plain min/max pair with no
///   way to spell a wrapped box, so a straddling footprint deliberately lands
///   out of range and is rejected by [`GeoPoint::is_on_earth`] rather than
///   folded into a bbox spanning the whole planet.
/// * A caller that genuinely wants a wrapped longitude calls
///   [`normalize_lon`].
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

/// Carry `lon` to the turn `near` is written in — the representation of the
/// same meridian that is closest to it.
///
/// **This is the map's fold, and it is not [`normalize_lon`].** The map's
/// centre, and every longitude `walkers::Projector::unproject` hands back from
/// it, live in a *continuous* frame that runs past ±180 as the map is panned
/// past the antimeridian; the data drawn on it — a station, a label, a radar
/// image's footprint — is written in the folded ±180 one. A datum more than
/// half a turn from where the pane is looking names the same ground as one just
/// off the opposite edge, and this picks the one the pane can see. Folding into
/// ±180 instead would move the *pane's* frame onto the data's, which is the
/// wrong direction: the pane has a viewport to keep and the datum has not.
///
/// A `near` or a `lon` that is not a number leaves `lon` alone: folding by
/// nonsense moves a datum that was already placed correctly.
///
/// **`round_ties_even`, and the tie is the whole reason.** A datum exactly half
/// a turn away is equally close in both representations, and `f64::round`
/// resolves that away from zero — so `fold_lon_near(0.0, -180.0)` would answer
/// `-360.0`, moving a prime meridian that was already as near as it can get and
/// putting a world-wide rect a turn off the glass. Ties to even leaves the
/// half-turn case where it is written. Every other case is a strict inequality
/// and both roundings agree.
///
/// `squallar-egui`'s `site_marker::fold_into_turn` is the same fold one stage
/// later, on a projected `x` against a screen centre. Either is correct; a
/// caller that still holds geography should prefer this one, because a point
/// carried before it is projected is also carried before every *difference* of
/// projections downstream of it. Note the two break the tie oppositely —
/// that one folds into a half-open `[centre - half, centre + half)` — which is
/// visible only for a datum exactly 180° out and is why they are not one
/// function.
#[inline]
pub fn fold_lon_near(lon: f64, near: f64) -> f64 {
    if !lon.is_finite() || !near.is_finite() {
        return lon;
    }
    lon + 360.0 * ((near - lon) / 360.0).round_ties_even()
}

/// Wrap a longitude into `[-180, 180)`, however many laps it is out by.
///
/// The one place that spelling lives. `great_circle_destination` deliberately
/// does not do this (see its contract); a caller that wants a wrapped longitude
/// asks for one here.
///
/// **`rem_euclid`, not a single `±360` correction.** The open-coded form in
/// `squallar-egui`'s `ui_section_edit::destination` subtracts 360 once, which is
/// correct for one lap and wrong for two — reachable by a long-range
/// destination or a caller that already added an offset. That site is the
/// obvious adoption candidate and is deliberately left alone here, because
/// changing it is a different crate's gate.
///
/// `180.0` wraps to `-180.0`: they are the same meridian, and picking one end
/// keeps the range half-open so a value cannot be spelled two ways.
#[inline]
pub fn normalize_lon(lon: f64) -> f64 {
    if !lon.is_finite() {
        return lon;
    }
    (lon + 180.0).rem_euclid(360.0) - 180.0
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

/// The tile grid's side at `zoom`, or zero where there is no grid.
///
/// **`checked_pow`, not `saturating_pow`.** `zoom` is a `u8` and the grid is
/// counted in `u32`, so zoom 32 and above name no grid at all — and saturating
/// answers `u32::MAX`, which is not a power of two and would have [`wrap_tile_x`]
/// carry a column onto a grid that does not exist and is the wrong size besides.
/// Zero is the honest answer and the callers here read it as one.
/// `walkers::mercator::total_tiles` says the same thing with an `Option`.
#[inline]
fn grid_side(zoom: u8) -> i64 {
    2u32.checked_pow(u32::from(zoom)).map_or(0, i64::from)
}

/// Convert longitude to a tile X index **without clamping it to the grid**.
///
/// The continuous counterpart of [`lon_to_tile_x`], and the one a wrapping map
/// walks its columns with: a viewport straddling the antimeridian names a
/// *negative* column west of the grid, or one past its eastern edge, and that
/// column is exactly where the far side of the world is drawn. Carry the answer
/// back onto the grid with [`wrap_tile_x`] before asking a source for it.
///
/// `as i64` saturates rather than wrapping, and takes `NaN` to zero, so a
/// longitude that is not a number names the prime meridian's column rather than
/// a column at the other end of `i64`.
pub fn lon_to_tile_x_unbounded(lon: f64, zoom: u8) -> i64 {
    let n = 2f64.powi(zoom as i32);
    (((lon + 180.0) / 360.0 * n).floor()) as i64
}

/// Carry a column index of any turn onto the grid at `zoom`.
///
/// `rem_euclid`, not `%`: the column west of zero is the grid's *last* column,
/// and the remainder operator answers `-1` there. Zoom 32 and above have no
/// grid to carry onto and answer column zero.
pub fn wrap_tile_x(x: i64, zoom: u8) -> u32 {
    let side = grid_side(zoom);
    if side <= 0 {
        return 0;
    }
    x.rem_euclid(side) as u32
}

/// Convert tile X index back to the western longitude of the tile.
pub fn tile_to_lon(x: u32, zoom: u8) -> f64 {
    tile_to_lon_unbounded(i64::from(x), zoom)
}

/// [`tile_to_lon`] for a column off either end of the grid, which reads as a
/// longitude off either end of the turn — the continuous frame the map's centre
/// and every projected position are already in.
pub fn tile_to_lon_unbounded(x: i64, zoom: u8) -> f64 {
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

    /// [`great_circle_destination`] hands back `site_lon + Δlon` and never wraps
    /// it, because a caller detects the antimeridian by exactly that.
    ///
    /// This is a **contract test, not a description**: `squallar-elevation`'s
    /// tile-cover guard reads an out-of-range longitude as "this box straddles
    /// the antimeridian". Normalising inside the function would leave that
    /// guard's own test green while the guard stopped firing, so the property
    /// is pinned at its source.
    #[test]
    fn the_destination_longitude_is_never_normalised() {
        // Due east from 179.9°E, far enough to cross: the raw answer is past
        // +180 and must stay there.
        let (_, lon) = great_circle_destination(0.0, 179.9, 90.0, 460.0);
        assert!(
            lon > 180.0,
            "crossing eastward gave {lon}, which has been wrapped; \
             squallar-elevation's antimeridian guard reads this value raw",
        );
        // And westward, past −180.
        let (_, lon) = great_circle_destination(0.0, -179.9, 270.0, 460.0);
        assert!(
            lon < -180.0,
            "crossing westward gave {lon}, which has been wrapped",
        );
        // Control: an ordinary destination that does not cross is in range, so
        // the two assertions above are about the wrap and not about the
        // function always answering out of range.
        let (_, lon) = great_circle_destination(39.0, -106.0, 90.0, 460.0);
        assert!((-180.0..=180.0).contains(&lon), "control gave {lon}");
    }

    /// [`normalize_lon`] is the wrap, and it survives more than one lap.
    #[test]
    fn a_longitude_wraps_into_the_half_open_range_however_many_laps_it_is_out() {
        for (raw, want) in [
            (0.0_f64, 0.0_f64),
            (179.9, 179.9),
            (-179.9, -179.9),
            (180.0, -180.0),
            (-180.0, -180.0),
            (184.03, -175.97),
            (-184.03, 175.97),
            // Two laps: the single `±360` correction spelled elsewhere in the
            // tree gets these wrong.
            (544.03, -175.97),
            (-544.03, 175.97),
        ] {
            let got = normalize_lon(raw);
            assert!(
                (got - want).abs() < 1e-9,
                "normalize_lon({raw}) = {got}, wanted {want}"
            );
            assert!((-180.0..180.0).contains(&got), "{got} left the range");
        }
        assert!(normalize_lon(f64::NAN).is_nan());
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

/// The wrap's own primitives: the fold that carries a datum into the pane's
/// turn, and the pair that takes a column off the grid and back onto it.
#[cfg(test)]
mod wrap_tests {
    use super::*;

    /// **A fold is a function of the two longitudes and nothing else**, and the
    /// ground it names never moves: whatever turn it lands in, the answer is the
    /// same meridian.
    #[test]
    fn the_fold_names_the_same_meridian_and_the_nearest_one() {
        for lon in [-179.9, -90.0, -0.1, 0.0, 45.0, 179.9, 180.0, 359.9] {
            for near in [
                -540.5, -186.0, -180.0, -97.2778, 0.0, 151.2, 180.0, 185.0, 540.5,
            ] {
                let folded = fold_lon_near(lon, near);

                // The same meridian: the two differ by a whole number of turns.
                let turns = (folded - lon) / 360.0;
                assert!(
                    (turns - turns.round()).abs() < 1e-9,
                    "fold({lon}, {near}) = {folded} is not a whole turn from {lon}"
                );

                // And the nearest one: no other representation is closer.
                let here = (folded - near).abs();
                for step in [-720.0, -360.0, 360.0, 720.0] {
                    assert!(
                        here <= (folded + step - near).abs() + 1e-9,
                        "fold({lon}, {near}) = {folded} is {here} from the pane, but \
                         {} is {} away",
                        folded + step,
                        (folded + step - near).abs()
                    );
                }
            }
        }
    }

    /// **The half-turn tie does not move.** `f64::round` breaks it away from
    /// zero, which would answer −360 for a prime meridian seen from a pane at
    /// −180 — as far away as it is possible to be while still being the nearest
    /// representation, and a whole world of misplacement for anything that is a
    /// rect rather than a point.
    #[test]
    fn a_datum_exactly_half_a_turn_out_is_left_where_it_is_written() {
        assert_eq!(fold_lon_near(0.0, -180.0), 0.0);
        assert_eq!(fold_lon_near(0.0, 180.0), 0.0);
        assert_eq!(fold_lon_near(180.0, 0.0), 180.0);
        assert_eq!(fold_lon_near(-180.0, 0.0), -180.0);

        // A hair either side of the tie is not a tie, and does move.
        assert_eq!(fold_lon_near(0.0, -180.0 - 1e-9), -360.0);
        assert_eq!(fold_lon_near(0.0, 180.0 + 1e-9), 360.0);
    }

    /// Nonsense in, the datum back out. Folding by a `NaN` would place a station
    /// that was already correct at `NaN`, which draws nothing anywhere.
    #[test]
    fn the_fold_refuses_a_value_that_is_not_a_number() {
        assert_eq!(fold_lon_near(17.0, f64::NAN), 17.0);
        assert_eq!(fold_lon_near(17.0, f64::INFINITY), 17.0);
        assert!(fold_lon_near(f64::NAN, 0.0).is_nan());
    }

    /// **The unbounded index is the clamped one wherever the clamp does not
    /// bite**, and carries on past the grid where it does.
    #[test]
    fn the_unbounded_column_agrees_with_the_clamped_one_inside_the_grid() {
        for zoom in [0u8, 1, 3, 8, 14, 20] {
            let side = i64::from(2u32.pow(u32::from(zoom)));
            for lon in [-179.999, -97.2778, -0.001, 0.0, 17.03664, 151.2093, 179.999] {
                assert_eq!(
                    lon_to_tile_x_unbounded(lon, zoom),
                    i64::from(lon_to_tile_x(lon, zoom)),
                    "lon {lon} at zoom {zoom}"
                );
            }

            // Off the grid, the clamp collapses and this does not.
            assert_eq!(lon_to_tile_x_unbounded(-180.0 - 360.0, zoom), -side);
            assert_eq!(lon_to_tile_x_unbounded(180.0, zoom), side);
            assert_eq!(lon_to_tile_x(-540.0, zoom), 0);
        }
    }

    /// A column of any turn is asked for on the grid, and it is the column a
    /// whole number of turns away — `rem_euclid`, so the column west of zero is
    /// the grid's last and not `-1`.
    #[test]
    fn a_column_of_any_turn_wraps_onto_the_grid() {
        for zoom in [0u8, 1, 4, 12] {
            let side = i64::from(2u32.pow(u32::from(zoom)));
            for column in -3 * side..3 * side {
                let wrapped = wrap_tile_x(column, zoom);
                assert!(i64::from(wrapped) < side, "column {column} at zoom {zoom}");
                assert_eq!(
                    (column - i64::from(wrapped)) % side,
                    0,
                    "column {column} at zoom {zoom} wrapped to a different tile"
                );
            }
        }

        // Zoom 32 and above name no grid at all; the answer is column zero
        // rather than a division by nothing.
        assert_eq!(wrap_tile_x(-7, 32), 0);
        assert_eq!(wrap_tile_x(9, 255), 0);
    }

    /// A column and the longitude it starts at are inverses, off the grid as
    /// well as on it.
    #[test]
    fn a_column_and_its_western_longitude_round_trip_off_the_grid() {
        for zoom in [0u8, 2, 7, 15] {
            for column in [-9i64, -1, 0, 1, 5, 100] {
                let lon = tile_to_lon_unbounded(column, zoom);
                assert_eq!(
                    lon_to_tile_x_unbounded(lon, zoom),
                    column,
                    "column {column} at zoom {zoom} came back as a different column"
                );
            }
        }

        // The bounded spelling is the unbounded one, on the grid.
        for zoom in [0u8, 3, 11] {
            for x in [0u32, 1, 2] {
                assert_eq!(
                    tile_to_lon(x, zoom),
                    tile_to_lon_unbounded(i64::from(x), zoom)
                );
            }
        }
    }

    /// A longitude that is not a number names the prime meridian's column, not
    /// a column at the other end of `i64`.
    #[test]
    fn an_unusable_longitude_names_a_column_a_walk_can_hold() {
        for zoom in [0u8, 6, 18] {
            assert_eq!(lon_to_tile_x_unbounded(f64::NAN, zoom), 0);
            assert!(lon_to_tile_x_unbounded(f64::INFINITY, zoom) > 0);
            assert!(lon_to_tile_x_unbounded(f64::NEG_INFINITY, zoom) < 0);
        }
    }
}
