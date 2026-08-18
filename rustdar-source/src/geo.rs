//! Shared horizontal geodesy: the Web Mercator projection's limit and its two
//! y-conversions.

use std::f64::consts::PI;

/// The latitude Web Mercator ends at: the one whose projected `y` is exactly
/// `π`, so the world is the square the tile grid needs it to be.
///
/// `2·atan(e^π) − π/2` in degrees. Quoted to the digits EPSG:3857 and the OSM
/// slippy-map note carry rather than to the `85.05` that copies of it in this
/// workspace were transcribed as — that truncation is 0.0011287798° short,
/// **125.51 m** of meridian, and while it is only ever a clamp bound the point
/// of a named limit is that it is the limit.
///
/// # Why the source substrate holds a map-projection constant
///
/// Because this is where the workspace's Web Mercator lives:
/// [`lat_rad_to_mercator_y`] and [`mercator_y_from_sin_lat`] are below it and
/// `rustdar-radar`'s `ImageBounds` is documented in terms of it. It is also
/// the lowest crate every caller can reach — `rustdar-egui`'s tile grid reads
/// it through `rustdar-radar`'s re-export while `rustdar-overlays`'s
/// rasterizer, which no longer depends on that crate at all, reads it here.
/// Two of the three copies this replaced existed *because* there was no such
/// place; it lived in `rustdar-radar`'s `types` until the substrate became
/// the floor both sides share.
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
