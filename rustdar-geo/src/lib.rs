//! The workspace's horizontal-geodesy floor: one sphere, one Web Mercator,
//! and the polygon/bounds vocabulary overlay features are built from.
//!
//! Everything here answers the same two-way question — where something is on
//! the ground, and where the ground is on the map — and this crate is where
//! the workspace answers it exactly once. [`EARTH_RADIUS_KM`] and the
//! [`KM_PER_DEGREE_LAT`] derived from it are the only sphere anything above
//! may convert between degrees and ground kilometres on;
//! [`MERCATOR_LAT_LIMIT_DEG`] with [`lat_rad_to_mercator_y`] and
//! [`mercator_y_from_sin_lat`] are its one Web Mercator; [`GeoBounds`] and
//! the polygon aliases are the shapes overlay features and viewport culling
//! speak in. `rustdar-radar/tests/geodesy_one_definition.rs` scans the whole
//! workspace to keep it that way — this file holds the two licensed defining
//! literals, and every other crate reaches them by re-export.
//!
//! # Horizontal geometry: 6371, spherical
//!
//! [`site_bearing_range_km`], [`great_circle_destination`] and
//! [`great_circle_point`] measure on a sphere of [`EARTH_RADIUS_KM`]
//! (6371 km) — deliberately the same constant `rustdar-radar`'s
//! `render_gate` projects gates with, so a line drawn on a plan view lands on
//! the ground the plan view put under the cursor. The map's hover readout
//! reads [`site_bearing_range_km`] for exactly that reason.
//!
//! The first two of those are a matched pair, inverse and direct, and the
//! plan view goes through the direct one: `render_gate` asks
//! [`great_circle_destination`] where a gate is and turns the answer into a
//! pixel. It used to walk `r·cos az` north and `r·sin az` east instead and
//! read those off as degrees — an equirectangular approximation of the same
//! question, worth 11.8 km at KTLX's 460 km reach and 17.9 km at KMSX's. The
//! table is at [`great_circle_destination`].
//!
//! `rustdar-radar`'s `ImageBounds` used to disagree, working in
//! `1.0 / 111.32` degrees per km — a 6378 km sphere, 0.11 % off this one,
//! which put the framing and everything hung off it (the range ring, the
//! volume floor, the region-drag preview) a quarter of a kilometre away from
//! the gates at the raster edge. It now works in [`KM_PER_DEGREE_LAT`],
//! which is this same [`EARTH_RADIUS_KM`] times `π/180`. There is one
//! horizontal sphere in the workspace, and the scan keeps refraction out of
//! its reach on purpose: `rustdar-radar`'s `beam::RE_EFF_KM` and the Level
//! III models are beam physics that happen to be derived from the same
//! figure, not second opinions about the planet.
//!
//! # One flat file, at the bottom of the graph
//!
//! The whole crate is `lib.rs` on purpose: the geodesy scan's two defining
//! licences key on this single path, and the flat root is what lets
//! `rustdar_source::geo`'s glob re-export republish the surface name for
//! name, so every path that resolved before this crate existed still does.
//! It sits below `rustdar-source` — pure geometry over `std`, no
//! dependencies at all — and `tests/charter.rs` pins that ceiling in
//! writing.

use std::f64::consts::PI;

/// Mean radius of Earth in kilometers — the IUGG mean radius, and the one
/// sphere every *horizontal* measurement in this workspace stands on.
///
/// # This is the horizontal-geodesy radius, not a propagation radius
///
/// Three different quantities in this workspace are spelled with a number
/// near 6371 and they are not interchangeable:
///
/// * **This one.** Degrees ↔ kilometres on the ground: where a gate is
///   painted, where the image bounds fall, how far the cursor is from the
///   site, where a cross-section's ground track runs. One sphere, because
///   the *only* thing that matters is that the data and the map under it
///   agree; see [`KM_PER_DEGREE_LAT`].
/// * **`rustdar-radar`'s `beam::RE_EFF_KM`**, `6371 · 4/3`. An atmospheric
///   refraction model that happens to be derived from the same figure.
///   Changing it is a change to beam physics, not to geodesy.
/// * **The `1.21 · 6371` Level III models** in `rustdar-radar`'s `eet`,
///   `dpprep` and `hca`. Each reproduces an RPG product bit-for-bit and each
///   says so at its own constant.
///
/// `rustdar-radar/tests/geodesy_one_definition.rs` is the guard that keeps
/// the first of those three from acquiring a fourth spelling; it carries the
/// reason for every other site in the workspace that names one of these
/// numbers.
pub const EARTH_RADIUS_KM: f64 = 6371.0;

/// Kilometres per degree of latitude on [`EARTH_RADIUS_KM`]: 111.194927 km.
///
/// **Derived, never written down.** This is the workspace's single conversion
/// between angle and ground distance, and it is an expression over
/// [`EARTH_RADIUS_KM`] precisely so that no caller can hold a different
/// planet from the one `rustdar-radar`'s `render::render_gate` paints gates
/// on. The
/// only copy that is not this expression is `volume.wgsl`'s, which cannot
/// see Rust; `rustdar-frontend`'s
/// `the_shaders_km_per_degree_is_the_radar_crates_own` pins that literal to
/// this value.
///
/// # It used to be 111.32, and 111.32 is the equatorial figure
///
/// `ImageBounds::from_radar_site` and everything downstream of it — the
/// plan-view range ring, the volume floor, the region-drag preview — spelled
/// `111.32`, which is a degree on a 6378.1 km (WGS-84 *equatorial*) sphere,
/// while the radar data itself was placed on 6371. The gap is 0.11 %: 0.26 km
/// at the 230 km raster edge, biased one way rather than averaging out, so
/// echoes sat consistently outside the geography drawn under them and the
/// error grew with range.
///
/// Neither figure is "correct" — a real degree of latitude runs 110.57 km at
/// the equator to 111.69 km at the poles — so the choice is consistency, not
/// accuracy. It resolved to 6371 because that is the sphere the *data* is on
/// (`render_gate`, [`site_bearing_range_km`],
/// `great_circle_point`, the voxel builder): framing follows the data rather
/// than the other way round. It is also the better of the two figures for
/// the latitudes this application serves — a degree at 35–45 °N is
/// 110.94–111.13 km, which 111.195 misses by ~0.1 % and 111.32 by ~0.25 %.
pub const KM_PER_DEGREE_LAT: f64 = EARTH_RADIUS_KM * PI / 180.0;

/// Initial great-circle bearing (degrees clockwise from true north, `0..360`)
/// and surface distance (km) from a radar site to a geographic point.
///
/// The radar-relative polar coordinates of a point the user picked on a map:
/// the bearing is the azimuth to steer, the distance is the ground range to
/// walk. Haversine distance on [`EARTH_RADIUS_KM`] and the standard forward
/// azimuth.
///
/// `ui_map::compute_hover_info_raw` used to compute the same pair inline for its
/// hover readout and now calls this. The de-duplication is provably not a change
/// to the readout: `the_hover_readouts_polar_coordinates_are_bit_identical_to_the_deleted_copy`
/// carries the deleted spelling and compares bit patterns, and the one place the
/// two forms *can* diverge — the clamp below, which the inline copy had no
/// counterpart for — is measured there too.
///
/// Distance is a *ground* range, so pairing it with a slant-range gate index
/// wants `rustdar-radar`'s `beam::slant_range_for_ground_km` in between.
pub fn site_bearing_range_km(site_lat: f64, site_lon: f64, lat: f64, lon: f64) -> (f64, f64) {
    let lat1 = site_lat.to_radians();
    let lon1 = site_lon.to_radians();
    let lat2 = lat.to_radians();
    let lon2 = lon.to_radians();
    let dlat = lat2 - lat1;
    let dlon = lon2 - lon1;

    // Clamped for the same reason `sites::distance_km` clamps: the haversine can
    // round to a hair *over* 1.0 for a near-antipodal pair, and `(1.0 - a).sqrt()`
    // is then `NaN` — which would come back as a `NaN` range rather than as the
    // 20 015 km half-circumference it should be. Measured: 3.7 % of antipodal
    // latitude pairs land above 1.0. Identity for anything closer than that.
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
/// The direct problem on the same [`EARTH_RADIUS_KM`] sphere the inverse one is
/// solved on, so `site_bearing_range_km` composed with this is the identity to
/// rounding (`a_bearing_and_range_round_trip_through_the_destination`, 3.9e-10 km
/// over a 4-site × 3600-azimuth × 6-range sweep).
///
/// # This is what a gate's position *is*
///
/// A radar measures a bearing and a distance and nothing else, so a gate's
/// geography is this function of them — there is no other definition available.
/// The plan view used to place gates by walking `r·cos az` north and
/// `r·sin az` east of the site and reading those off as degrees, which is an
/// equirectangular approximation: it is the first-order expansion of the
/// formula below and it drops the whole second-order term. Two things go wrong
/// with it, both outward on the diagonals and both growing with the square of
/// the range:
///
/// | site | range | worst displacement | worst range overrun |
/// |---|---|---:|---:|
/// | KTLX, 35.33°N |  88.8 km |  0.44 km | 0.17 km |
/// | KTLX, 35.33°N | 460.0 km | 11.79 km | 4.81 km |
/// | KMSX, 47.04°N | 460.0 km | 17.87 km | 7.27 km |
///
/// The overrun column is the one that had a consumer: a gate at the raster's
/// declared extent came out *past* it, so the number a render reports did not
/// bound the picture it hands over. The displacement column is the one a viewer
/// sees — an echo drawn 12 km from the ground it fell on.
///
/// Distance is a **ground** range, matching [`site_bearing_range_km`]'s output
/// and `rustdar-radar`'s `beam::ground_range_km`'s; a caller holding a slant
/// range applies `ground_range_km` first.
///
/// `rustdar-radar` `render`'s rasterizers do not, and this line used to claim
/// they did. They multiply by a hoisted `cos e` instead, which was the same
/// answer while `ground_range_km` was the tangent plane and is no longer —
/// `rustdar-radar`'s `beam` module doc's "Nothing spells the tangent plane any
/// more" measures what that is worth and why the hoist cannot simply be
/// swapped for a call.
///
/// A range past half the circumference wraps over the pole and keeps going,
/// which is what the spherical formula means and not something to guard: no
/// radar reaches 20 015 km. `NaN` in either coordinate propagates.
pub fn great_circle_destination(
    site_lat: f64,
    site_lon: f64,
    bearing_deg: f64,
    ground_range_km: f64,
) -> (f64, f64) {
    let (sin_lat1, cos_lat1) = site_lat.to_radians().sin_cos();
    let (sin_az, cos_az) = bearing_deg.to_radians().sin_cos();
    let (sin_d, cos_d) = (ground_range_km / EARTH_RADIUS_KM).sin_cos();

    // Clamped for the same reason the haversine is: the sum can round a hair
    // past ±1 for a range landing on a pole, and `asin` of that is `NaN`.
    let sin_lat2 = (sin_lat1 * cos_d + cos_lat1 * sin_d * cos_az).clamp(-1.0, 1.0);
    let dlon = (sin_az * sin_d * cos_lat1).atan2(cos_d - sin_lat1 * sin_lat2);

    (sin_lat2.asin().to_degrees(), site_lon + dlon.to_degrees())
}

/// The point a fraction `t` of the way from `a` to `b` along their great
/// circle, as `(lat, lon)` in degrees. `t` outside `0..=1` extrapolates along
/// the same circle.
///
/// Spherical interpolation, so the parameter is **angle** and the sphere's
/// radius cancels out entirely — which is what makes it exact rather than
/// merely consistent with [`site_bearing_range_km`]: a point at `t` along a
/// line starting at the site sits at exactly `t` of that line's ground range
/// (`a_fraction_along_a_line_is_that_fraction_of_its_ground_range`). A
/// latitude-longitude lerp has neither property and bends visibly over a
/// 460 km section.
///
/// Returns `a` when the two endpoints are coincident or antipodal, neither of
/// which names a unique great circle. A cross-section never hits either, but
/// both are reachable by hand and both fail *plausibly* rather than loudly if
/// left alone: a coincident pair divides by zero, and an antipodal pair returns
/// `(0.0, 0.0)`, a real place in the Gulf of Guinea. The guard's derivation and
/// its 1.519 m reach are in the comment at the test itself.
///
/// A non-finite input is **not** caught. `hav` is then `NaN`, which fails both
/// of the guard's comparisons, so `NaN` propagates to the result — the honest
/// answer for a coordinate that was never a coordinate.
pub fn great_circle_point(a: (f64, f64), b: (f64, f64), t: f64) -> (f64, f64) {
    let (lat1, lon1) = (a.0.to_radians(), a.1.to_radians());
    let (lat2, lon2) = (b.0.to_radians(), b.1.to_radians());

    let dlat = lat2 - lat1;
    let dlon = lon2 - lon1;
    // Clamped for the reason given in `site_bearing_range_km`.
    let hav = ((dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2))
        .clamp(0.0, 1.0);
    let d = 2.0 * hav.sqrt().atan2((1.0 - hav).sqrt());

    // Refuse on `hav`, not on `sin d`, and with a threshold derived from the
    // conditioning rather than from zero.
    //
    // `hav` is computed straight from the inputs and carries ~1 ulp of error.
    // `d` does not: with `u = 1 − hav`, `d = π − 2√u + O(u^1.5)`, so `hav`'s
    // last ulp lands on `d` amplified to ≈ ε/√u while the divisor `sin d` is
    // only ≈ 2√u. The divisor's relative error is therefore ≈ ε/(2u), which
    // passes 1 % once `u` drops under ~50ε — the direction of the "great
    // circle" is noise below that, not merely undefined.
    //
    // Testing `d` or `sin d` instead is what a first attempt does and it does
    // not work, because a truly antipodal pair does not reliably land `hav` on
    // exactly 1.0. Measured over 3602 antipodal latitude pairs: 2922 (81.1 %)
    // give exactly 1.0, 648 (18.0 %) one ulp below, 32 (0.89 %) two ulps below.
    // `√(1 − hav)` turns even one ulp into `sin d ≈ 2e-8`, and two into
    // `≈ 3e-8` — eight orders above `f64::EPSILON`. So `sin d == 0.0` catches
    // **0** of the 3602, `|sin d| < f64::EPSILON` catches the 2922 that landed
    // on 1.0 and misses all 680 that did not, and only the `hav` test
    // catches every one. What leaks returns `(0.0, 0.0)` — null island, a real
    // place in the Gulf of Guinea — which is the failure mode this guard exists
    // to prevent.
    //
    // Cost in reach: the guard withdraws below a 1.519 m separation, 165× finer
    // than one 250 m gate.
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

/// Ring of (latitude, longitude) points. First ring is exterior, rest are holes.
pub type GeoPolygonRing = Vec<(f64, f64)>;

pub type GeoPolygon = Vec<GeoPolygonRing>;

/// Geographic bounding box for viewport culling.
///
/// `PartialEq` because the box rides inside
/// `rustdar_frontend::offload::JobRequest`, whose wire round-trip tests compare
/// whole requests; it is derived — four `f64` comparisons — and carries the
/// usual `f64` caveat that `NaN != NaN`.
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

    /// Whether `(lat, lon)` is inside the box, **inclusive on all four
    /// edges** — a point exactly on an edge is contained, matching
    /// [`GeoBounds::intersects`]'s own inclusivity.
    ///
    /// Spelled as the negation of the reject test rather than as a
    /// conjunction of `>=`/`<=` on purpose: the two hand-rolled copies this
    /// replaced (the HRRR hover's and the station-picker's) were both
    /// written as "reject when any coordinate is out", and under a `NaN`
    /// coordinate the two spellings differ — every ordering against `NaN` is
    /// false, so the reject form lets a `NaN` point through to the caller's
    /// own arithmetic (where it fails the distance test or projects to a
    /// `NaN` the next check drops) while the conjunction form would silently
    /// swallow it here. Convergence has to be the identity, so this keeps
    /// the reject form's answer.
    pub fn contains_point(&self, lat: f64, lon: f64) -> bool {
        !(lat < self.min_lat || lat > self.max_lat || lon < self.min_lon || lon > self.max_lon)
    }

    /// The workspace's one min/max bounds fold: the tightest box around
    /// every `(lat, lon)` yielded, `None` when the iterator yields nothing.
    ///
    /// `f64::min`/`f64::max` never adopt a `NaN` — a `NaN` vertex leaves
    /// every edge where it was, exactly as the `if lat < min_lat` spelling
    /// it also replaced (in the HRRR domain-extent pass) behaved: an
    /// ordering against `NaN` is false, so neither form admits one. The
    /// `None` on emptiness is what the streaming copy did *not* have — it
    /// fell through to a `{MAX, MIN, MAX, MIN}` box for its caller to trip
    /// over; refusal is the honest form, and the caller states its own
    /// error.
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

/// The latitude Web Mercator ends at: the one whose projected `y` is exactly
/// `π`, so the world is the square the tile grid needs it to be.
///
/// `2·atan(e^π) − π/2` in degrees. Quoted to the digits EPSG:3857 and the OSM
/// slippy-map note carry rather than to the `85.05` that copies of it in this
/// workspace were transcribed as — that truncation is 0.0011287798° short,
/// **125.51 m** of meridian, and while it is only ever a clamp bound the point
/// of a named limit is that it is the limit.
///
/// # Why the geodesy floor holds a map-projection constant
///
/// Because this is where the workspace's Web Mercator lives:
/// [`lat_rad_to_mercator_y`] and [`mercator_y_from_sin_lat`] are below it and
/// `rustdar-radar`'s `ImageBounds` is documented in terms of it. It is also
/// the lowest crate every caller can reach — `rustdar-egui`'s tile grid reads
/// it through `rustdar-radar`'s re-export while `rustdar-overlays`'s
/// rasterizer, which no longer depends on that crate at all, reads it through
/// `rustdar_source::geo`'s. Two of the three copies this replaced existed
/// *because* there was no such place; it lived in `rustdar-radar`'s `types`
/// until the substrate became the floor both sides share, and moved one crate
/// further down when the floor itself became this crate.
///
/// # It is a domain bound, not a clamp every caller must apply
///
/// `tiles::lat_to_tile_y` needs no branch on it — its index clamp already
/// carries every latitude past this to the edge row — and
/// `render::rasterize`'s `MercatorBounds::from_geo` needs none either, because
/// `overlay_cache::OverlayTexturePlan::coverage` has already clamped the same
/// bounds to this value before the rasterizer is handed them. It is here
/// because the projection's domain should be nameable, and because a caller
/// that does clamp must clamp to the same edge the tiles under it end at.
///
/// `rustdar-radar/tests/geodesy_one_definition.rs` is the guard that stops a
/// fourth copy appearing.
pub const MERCATOR_LAT_LIMIT_DEG: f64 = 85.051_128_779_806_6;

#[inline]
pub fn lat_rad_to_mercator_y(lat_rad: f64) -> f64 {
    (PI / 4.0 + lat_rad / 2.0).tan().ln()
}

/// The same Web Mercator y, reached from `sin φ` instead of from `φ`.
///
/// `ln(tan(π/4 + φ/2)) ≡ artanh(sin φ)`, a standard identity of the
/// projection and not an approximation of it, so this is
/// [`lat_rad_to_mercator_y`] and not a second convention;
/// `rustdar-radar`'s
/// `types::tests::the_mercator_y_from_a_sine_is_the_one_from_an_angle`
/// measures the two against each other over every tenth of a degree either
/// side of the equator.
///
/// It exists because `rustdar_radar::render`'s gate loop now gets `sin φ` for
/// free. A gate's latitude comes out of `beam::great_circle_destination`'s
/// arithmetic as a sine, and the only thing the rasterizer does with a
/// latitude is turn it into a row — so going through the angle would mean an
/// `asin` to recover it and a `tan` to undo the `asin`, two transcendentals
/// per sample to arrive where one `ln` already is. That loop runs ~28 M times
/// per frame and the module doc for `render::RenderBuffers` measures the
/// Mercator conversion as most of it — which is also why both helpers keep
/// `#[inline]`: without the attribute nothing cross-crate can inline them,
/// and no test would catch the regression.
///
/// `sin φ` of exactly ±1 is a pole, where the projection is genuinely
/// infinite; this returns ±∞ there, as the angle form does. Outside ±1 —
/// which no sine is — it returns `NaN`.
#[inline]
pub fn mercator_y_from_sin_lat(sin_lat: f64) -> f64 {
    // `0.5 · ln((1 + s)/(1 − s))` rather than `s.atanh()`: identical in exact
    // arithmetic and within an ulp in floating point, and this spelling is the
    // one that survives `s == 1.0` as `+∞` rather than depending on a libm's
    // choice there.
    0.5 * ((1.0 + sin_lat) / (1.0 - sin_lat)).ln()
}

/// The one inverse Web Mercator: the latitude, in **radians**, whose
/// [`lat_rad_to_mercator_y`] is `merc_y` — the Gudermannian, `atan(sinh y)`.
///
/// # Why this spelling and not `2·atan(eʸ) − π/2`
///
/// The projection has two textbook inverses and the workspace held one copy
/// of each: `rustdar-egui`'s tile math spells `atan ∘ sinh` (the form
/// `walkers` and every slippy-tile reference implementation use, and the one
/// the mercantile reference vectors in `tiles/tests.rs` pin), while
/// `rustdar-overlays`' rasterizer spelled `2·atan(eʸ) − π/2`. They agree to
/// an ulp across the projection's working range, so the choice is which
/// copy's bits survive — and it is the tile form, for two reasons. It keeps
/// the tile path bit-identical, so the reference vectors that anchor the
/// grid to mercantile keep passing unedited when that path delegates here.
/// And it computes a pole-adjacent latitude directly rather than as the
/// small difference of two quantities the size of `π` — `atan(eʸ)` saturates
/// toward `π/2` as `y` grows, so the doubled-and-shifted form reaches a
/// latitude near `+π/2` by cancellation while `atan(sinh y)` lands on it in
/// one rounding.
///
/// Total, like the forward function: `±∞` maps to `±π/2` (the poles the
/// forward form sends to `±∞`) and `NaN` propagates. Degrees are the
/// caller's own `.to_degrees()`, matching [`lat_rad_to_mercator_y`] taking
/// radians.
pub fn mercator_y_to_lat_rad(merc_y: f64) -> f64 {
    merc_y.sinh().atan()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bit-level distance between two finite `f64`s, in units in the last
    /// place: their positions in the monotonic order all finite doubles
    /// admit. Sign-symmetric (`-0.0` and `+0.0` are 0 apart).
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

    /// [`mercator_y_to_lat_rad`] inverts [`lat_rad_to_mercator_y`] to within
    /// 4 ulps at ±60°, ±45° and the equator — latitudes chosen to sit outside
    /// every literal band the geodesy scan guards, so this file stays free of
    /// banded numbers.
    ///
    /// Measured on this pipeline: ±45° and −60° round-trip **exactly**, +60°
    /// is 1 ulp out; the 4-ulp ceiling is headroom over that, not a hope.
    ///
    /// The equator is asserted in absolute terms because the ulp metric
    /// degenerates at zero: the forward projection computes `ln(tan(π/4))`,
    /// whose result carries the ~2⁻⁵³ absolute rounding floor of `tan` near
    /// 1, and every representable double between 0 and that floor counts as
    /// an ulp — no finite-precision implementation could meet a raw 4-ulp
    /// bound there. Measured: the round trip lands at −2⁻⁵³ radians exactly
    /// (half an epsilon, ~0.7 picometres of meridian); the bound allows four
    /// times that, mirroring the headroom the ulp arm gets.
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
