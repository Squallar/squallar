//! Beam geometry: the one place the crate turns a radar's polar coordinates
//! into height, ground range and geography, and back.
//!
//! Everything a display draws — the plan view's gates, an echo top, a
//! cross-section's rows and columns, a voxel's centre — has to agree about
//! where a beam *is*. Before this module the answer lived in five places with
//! two different earth radii and no inverse at all, so a product could sit a
//! gate away from the product beside it with nothing in the code saying which
//! was right. The functions below are that single answer.
//!
//! # Earth model: 4/3, quadratic
//!
//! [`RE_EFF_KM`] is the standard-atmosphere effective earth radius, `4/3 · Re`,
//! which folds the beam's downward refraction into a straight ray over a
//! larger sphere. It is written as the expression `6371.0 * 4.0 / 3.0` and not
//! as `8494.667`, because [`height_km`]'s output is pinned bit-exactly by
//! `volumetric::tests::golden_echo_tops_grid_is_pinned` (and by four more
//! assertions of the same digest in [`crate::chunks`]) — a rounded literal
//! moves the digest.
//!
//! This is deliberately **not** the `1.21 · Re` model that [`crate::eet`],
//! [`crate::dpprep`] and [`crate::hca`]'s melting-layer code use. Those three
//! exist to reproduce an RPG Level III product bit-for-bit, and the RPG's
//! `a313e1.ftn` picks 1.21 for that product family; being faithful to the
//! source is the whole point there, and each of those modules says so at its
//! own constant. Nothing in this module has a Level III twin. What it does
//! have is neighbours on screen: a cross-section drawn beside an echo-tops
//! plan view, a voxel grid orbited over the same volume. Those must agree with
//! each other, so they all use the model the crate *draws* beams with. On a
//! 0.5° tilt the two models are 0.199 kft apart at 100.5 km and 1.041 kft — a
//! full EET data level — apart at 230 km, which is exactly the size of error
//! that looks plausible and is wrong. (`eet::tests::
//! beam_altitudes_use_the_rpgs_own_refraction_constant` covers the 100.5 km
//! figure only, and as a `> 0.15 kft` lower bound rather than a value; the
//! 230 km figure is computed here and asserted nowhere, so treat it as
//! arithmetic rather than as a pin.)
//!
//! # The quadratic, and what it approximates
//!
//! [`height_km`] is the second-order form `r·sin e + r²/(2·Rₑ)`. The exact
//! spherical height on the same effective sphere — the form
//! [`nexrad_model::geo::RadarCoordinateSystem::polar_to_geo`] uses, over the
//! same `6_371_000.0 * 4.0 / 3.0` metres — is
//!
//! ```text
//! h = √(r² + Rₑ² + 2·r·Rₑ·sin e) − Rₑ
//! ```
//!
//! The quadratic is kept for two reasons: it is what the shipped products
//! already compute (so lifting it here is a refactor and not a change of
//! answer), and it has the closed-form inverse [`slant_range_for_height_km`],
//! which a cross-section needs once per output row.
//!
//! **The residual, measured.** ~1.54 m at 230 km / 0.5°, ~32.84 m at
//! 70 km / 19.5° — both far under one 250 m gate.
//!
//! **But the bound is domain-dependent, and the domain that governs it is
//! height, not range.** At 230 km / 19.5° the residual is ~372 m, *larger than
//! one 250 m gate*. That corner is only harmless because the beam is at 79.9 km
//! there — four times above anything a weather display plots, and beyond the
//! reach of the range-truncated upper cuts that carry those elevations.
//!
//! The reason it is height and not range is an algebraic identity, exact by
//! construction rather than observed. The spherical form's radicand *is* the
//! quadratic height in disguise:
//!
//! ```text
//! r² + Rₑ² + 2·r·Rₑ·sin e  ≡  Rₑ² + 2·Rₑ·h_quad
//! ```
//!
//! — expand `Rₑ² + 2·Rₑ·(r·sin e + r²/(2·Rₑ))` and the `r²` and `2·r·Rₑ·sin e`
//! terms fall out. So `h_sphere = √(Rₑ² + 2·Rₑ·h_quad) − Rₑ` is a function of
//! `h_quad` **alone**, with `r` and `e` appearing nowhere but inside it, and
//! writing `q = h_quad/Rₑ`:
//!
//! ```text
//! h_quad − h_sphere = Rₑ·((1 + q) − √(1 + 2·q))  ≈  h_quad²/(2·Rₑ)
//! ```
//!
//! `the_beam_height_residual_depends_only_on_the_height` measures this against
//! the two forms evaluated independently, to `4·ε·Rₑ` = 7.5e-12 km — which is
//! the floor of the *measurement*, not of the identity: `h_sphere` subtracts Rₑ
//! from a root a few km larger, so it cannot be evaluated more precisely than
//! `ε·Rₑ` ≈ 1.9e-12 km however exact the algebra is.
//!
//! So the usable statement is a ceiling in **kilometres of altitude**: the
//! residual reaches 250 m at 65.42 km and is at most **23.49 m anywhere below
//! 20 km**, which is the height axis a cross-section actually draws. Anyone
//! extending this module's domain should re-derive the bound from that ceiling
//! rather than trusting "always under one gate", which stops being true the
//! moment a caller wants heights the troposphere does not have.
//!
//! # Horizontal geometry: 6371, spherical
//!
//! [`site_bearing_range_km`], [`great_circle_destination`] and
//! [`great_circle_point`] measure on a sphere of
//! [`crate::types::EARTH_RADIUS_KM`] (6371 km) — deliberately the same
//! constant [`crate::render`]'s `render_gate` projects gates with, so a line
//! drawn on a plan view lands on the ground the plan view put under the
//! cursor. The map's hover readout reads [`site_bearing_range_km`] for exactly
//! that reason.
//!
//! The first two of those are a matched pair, inverse and direct, and the plan
//! view goes through the direct one: `render_gate` asks
//! [`great_circle_destination`] where a gate is and turns the answer into a
//! pixel. It used to walk `r·cos az` north and `r·sin az` east instead and read
//! those off as degrees — an equirectangular approximation of the same
//! question, worth 11.8 km at KTLX's 460 km reach and 17.9 km at KMSX's. The
//! table is at [`great_circle_destination`].
//!
//! [`crate::types::ImageBounds`] used to disagree, working in `1.0 / 111.32`
//! degrees per km — a 6378 km sphere, 0.11 % off this one, which put the
//! framing and everything hung off it (the range ring, the volume floor, the
//! region-drag preview) a quarter of a kilometre away from the gates at the
//! raster edge. It now works in [`crate::types::KM_PER_DEGREE_LAT`], which is
//! this same [`crate::types::EARTH_RADIUS_KM`] times `π/180`. There is one
//! horizontal sphere in the workspace and
//! `rustdar-radar/tests/geodesy_one_definition.rs` is what keeps it that way —
//! including keeping [`RE_EFF_KM`] below out of its reach, since that is
//! refraction and not geodesy.
//!
//! [`ground_range_km`] is the tangent-plane projection `r·cos e`, and not the
//! spherical arc `Rₑ·asin(r·cos e/(Rₑ + h))` that `polar_to_geo` returns. Those
//! differ by ~110 m at 230 km / 0.5° and ~182 m at 70 km / 19.5° — the same
//! order as the beam-height residual, and in the same direction for every
//! consumer, which is what makes it a consistency choice rather than an
//! accuracy claim. It is *not* the same order as the equirectangular placement
//! that used to sit downstream of it, which was a hundred times larger; that
//! one is gone and this one is deliberate.
//!
//! Every drawn product in this crate now shares that choice. The plan view's
//! four per-tilt rasterizers hoist `cos e` of their sweep's median elevation
//! and paint at the ground range, the sampler converts a ground range back to
//! a slant one before reading a gate, and a section drawn from the sampler
//! therefore registers against the map above it at any tilt. What is left
//! between them is this paragraph's own subject — tangent plane against
//! spherical arc, and the 6371-vs-6378 inconsistency [`crate::types::
//! ImageBounds`] carries — which is a tenth of a pixel, not the 18 px that
//! `cos e` was worth at 19.5°.

use crate::types::EARTH_RADIUS_KM;

/// Effective earth radius under the standard 4/3 refraction model, km.
///
/// Written as an expression rather than `8494.667` on purpose: see the module
/// doc. Formerly duplicated in `volumetric` (as `6371.0 * 4.0 / 3.0`) and in
/// `nrot` (as `4.0 / 3.0 * 6371.0`); both associations round to the same bits,
/// which `the_shared_effective_earth_radius_is_bit_identical_to_both_deleted_copies`
/// pins so the de-duplication is provably not a numeric change.
pub const RE_EFF_KM: f64 = 6371.0 * 4.0 / 3.0;

/// Half-power beamwidth of the WSR-88D antenna, degrees. A tilt's beam bottom
/// and top sit half of this below and above its centre elevation.
pub const WSR88D_HALF_POWER_BEAMWIDTH_DEG: f64 = 0.95;

/// Half-power beamwidth of the TDWR antenna, degrees.
///
/// **Sourced, not inferred.** NOAA's NCEI states it directly in its TDWR
/// product description — "Each radial in TDWR has a beam width of 0.55
/// degrees" (<https://www.ncei.noaa.gov/products/radar/terminal-doppler-weather-radar>,
/// read 2026-08-11) — and it is the figure of the system's design paper,
/// Michelson, Shrader and Wieler, "Terminal Doppler Weather Radar",
/// *Microwave Journal* **33** (1990), 139.
///
/// It is also what the hardware has to give. The TDWR is a 25-foot reflector
/// at C band (5.60–5.65 GHz, λ ≈ 53 mm), and `1.27·λ/D` puts an
/// evenly-illuminated dish of that size at 0.51°; ordinary illumination taper
/// widens it to about this. The WSR-88D reaches 0.95° from a 28-foot
/// reflector because it is at S band, where the wavelength is nearly twice as
/// long — the TDWR's narrower beam is the whole reason it can watch one
/// airport closely.
pub const TDWR_HALF_POWER_BEAMWIDTH_DEG: f64 = 0.55;

/// The half-power beamwidth, degrees, of whichever network the radar nearest
/// `lat`/`lon` belongs to.
///
/// A lookup rather than a constant because the two networks differ by nearly
/// a factor of two and this crate draws both. The nearest-site indirection is
/// the same one [`crate::eet::radar_height_ft_near`] uses and for the same
/// reason: a render path holds the site's coordinates, not its identifier,
/// and every row of [`crate::sites::radars()`] is its own nearest neighbour, so
/// this resolves to the site the caller meant rather than guessing.
///
/// A non-finite coordinate, or an empty table, answers the WSR-88D's. That is
/// the wider of the two, so an unknown site gets the more conservative beam
/// rather than one claiming a resolution nothing has established.
pub fn half_power_beamwidth_deg_near(lat: f64, lon: f64) -> f64 {
    match crate::sites::nearest_radar_site(lat, lon) {
        Some((site, _)) if site.is_tdwr() => TDWR_HALF_POWER_BEAMWIDTH_DEG,
        _ => WSR88D_HALF_POWER_BEAMWIDTH_DEG,
    }
}

/// Beam-centre height above the radar, km, at a slant range and elevation.
///
/// The vertical coordinate every drawn product in this crate shares. Heights
/// are **above the antenna**, not above MSL; a caller wanting MSL adds the
/// site's feedhorn height itself — [`crate::eet::radar_height_ft_near`] on
/// [`crate::sites::Datum::Feedhorn`], which is the antenna. Adding the ground
/// under the tower instead lands a whole tower low, which is why that lookup
/// makes the caller name which one it means.
#[inline]
pub fn height_km(slant_range_km: f64, elev_deg: f64) -> f64 {
    // Transcribed character-for-character from the `volumetric::beam_height_km`
    // this replaced, association order included, because five bit-exact digest
    // assertions pin its output. `range_km` is bound rather than substituted so
    // the expression below is *literally* the shipped one; do not "simplify" it
    // to `powi(2)` or reassociate the divide.
    let range_km = slant_range_km;
    let el = elev_deg.to_radians();
    range_km * el.sin() + range_km * range_km / (2.0 * RE_EFF_KM)
}

/// The slant range at which a tilt's beam centre reaches `height_km` above the
/// radar — the exact algebraic inverse of [`height_km`].
///
/// `Rₑ·(√(sin²e + 2h/Rₑ) − sin e)`, the 4/3-model counterpart of
/// `hca::ml_range_from_height`'s 1.21-model `Compute_range_from_height`. A
/// cross-section needs one of these per output row, which is why the quadratic
/// height form is worth keeping over the spherical one.
///
/// Returns `NaN` where `sin²e + 2h/Rₑ` goes negative, i.e. below
/// `h = −Rₑ·sin²e/2`: no ascending beam reaches those heights at any range.
/// The bound is 0 km at 0° elevation and −0.32 km at 0.5°, so it is only
/// reachable by asking for a height *below the antenna* — which a section
/// axis anchored at the site elevation never does, and a caller that might
/// should check for finiteness rather than trust the range it gets back.
#[inline]
pub fn slant_range_for_height_km(height_km: f64, elev_deg: f64) -> f64 {
    let s = elev_deg.to_radians().sin();
    RE_EFF_KM * ((s * s + 2.0 * height_km / RE_EFF_KM).sqrt() - s)
}

/// Ground range, km: the horizontal distance from the site to the point under
/// a gate at `slant_range_km` on a tilt of `elev_deg`.
///
/// Tangent-plane `r·cos e`, per the module doc. The factor is 0.09 % at 2.4°
/// and 5.7 % at 19.5° — 0.2 km and 4.0 km at those tilts' plotted extents —
/// and **50 % at the 60° a TDWR's VCP 80 climbs to**, which is why the plan
/// view applies it rather than treating it as small.
#[inline]
pub fn ground_range_km(slant_range_km: f64, elev_deg: f64) -> f64 {
    slant_range_km * elev_deg.to_radians().cos()
}

/// The slant range whose gate sits over `ground_range_km` — the inverse of
/// [`ground_range_km`].
///
/// Diverges at 90°, where a vertically pointing beam covers no ground; the
/// WSR-88D's highest cut is 19.5°, so no production caller is near it.
#[inline]
pub fn slant_range_for_ground_km(ground_range_km: f64, elev_deg: f64) -> f64 {
    ground_range_km / elev_deg.to_radians().cos()
}

/// Beam-centre height above the radar, km, over a point at `ground_range_km`
/// from the site on a tilt of `elev_deg`.
///
/// `s·tan e + s²/(2·Rₑ·cos²e)`, which is [`height_km`] composed with
/// [`slant_range_for_ground_km`] with the division folded in. Written closed
/// form because a cross-section evaluates it per output column. The two
/// spellings are *not* bit-identical — the folded form divides once where the
/// composition divides twice — so
/// `the_ground_range_height_is_the_slant_range_height_over_the_same_point`
/// measures the gap rather than assuming it away: 2.8e-14 km (28 pm) at worst
/// over 1..460 km × the VCP 212 ladder.
#[inline]
pub fn height_at_ground_km(ground_range_km: f64, elev_deg: f64) -> f64 {
    let el = elev_deg.to_radians();
    let cos_el = el.cos();
    ground_range_km * el.tan()
        + ground_range_km * ground_range_km / (2.0 * RE_EFF_KM * cos_el * cos_el)
}

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
/// wants [`slant_range_for_ground_km`] in between.
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
/// and [`ground_range_km`]'s; a caller holding a slant range applies
/// [`ground_range_km`] first, as every rasterizer in [`crate::render`] does.
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

#[cfg(test)]
mod tests;
