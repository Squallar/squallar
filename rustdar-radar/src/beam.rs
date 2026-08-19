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
//! The quadratic is kept for **one** reason: it is what the shipped products
//! already compute, so lifting it here is a refactor and not a change of
//! answer, which five bit-exact digest assertions hold it to.
//!
//! It is *not* kept for being the invertible one, and a previous version of
//! this note said it was. The spherical form inverts in closed form too —
//! `r = √((Rₑ + h)² − Rₑ²·cos²e) − Rₑ·sin e`, one root and two trig calls,
//! the same shape of work as [`slant_range_for_height_km`]. "A cross-section
//! needs a closed form once per output row" is true and argues for nothing,
//! because both forms have one. Anyone weighing the two should weigh the pins
//! and the 23.49 m bound below, which are the real terms.
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
//! # Horizontal geometry: 6371, spherical — the sphere ops live in `rustdar-geo` now
//!
//! [`rustdar_geo::site_bearing_range_km`],
//! [`rustdar_geo::great_circle_destination`] and
//! [`rustdar_geo::great_circle_point`] measure on a sphere of
//! [`rustdar_geo::EARTH_RADIUS_KM`] (6371 km) — deliberately the same
//! constant [`crate::render`]'s `render_gate` projects gates with, so a line
//! drawn on a plan view lands on the ground the plan view put under the
//! cursor. The map's hover readout reads `site_bearing_range_km` for exactly
//! that reason.
//!
//! All three moved **verbatim** to `rustdar-geo` — the workspace's geometry
//! floor, below even `rustdar-source` — at WO-G1 (the `beam::` re-export
//! shims died at WO-G4; `rustdar_geo::` is the one spelling). Their docs,
//! the equirectangular error table and the antipodal guard's derivation
//! travelled with them; the history stays here too, because it is this
//! module's history:
//!
//! The first two of those are a matched pair, inverse and direct, and the plan
//! view goes through the direct one: `render_gate` asks
//! [`rustdar_geo::great_circle_destination`] where a gate is and turns the answer into a
//! pixel. It used to walk `r·cos az` north and `r·sin az` east instead and read
//! those off as degrees — an equirectangular approximation of the same
//! question, worth 11.8 km at KTLX's 460 km reach and 17.9 km at KMSX's. The
//! table is at [`rustdar_geo::great_circle_destination`].
//!
//! [`crate::types::ImageBounds`] used to disagree, working in `1.0 / 111.32`
//! degrees per km — a 6378 km sphere, 0.11 % off this one, which put the
//! framing and everything hung off it (the range ring, the volume floor, the
//! region-drag preview) a quarter of a kilometre away from the gates at the
//! raster edge. It now works in [`rustdar_geo::KM_PER_DEGREE_LAT`], which is
//! this same [`rustdar_geo::EARTH_RADIUS_KM`] times `π/180`. There is one
//! horizontal sphere in the workspace and
//! `rustdar-radar/tests/geodesy_one_definition.rs` is what keeps it that way —
//! including keeping [`RE_EFF_KM`] below out of its reach, since that is
//! refraction and not geodesy.
//!
//! [`ground_range_km`] is the spherical arc on that same effective sphere,
//!
//! ```text
//! s = Rₑ·asin(r·cos e/(Rₑ + h)),   h = √(r² + Rₑ² + 2·r·Rₑ·sin e) − Rₑ
//! ```
//!
//! which is the exact form and the one
//! [`nexrad_model::geo::RadarCoordinateSystem::polar_to_geo`] computes, over a
//! radius written `6_371_000.0 * 4.0 / 3.0` metres that is [`RE_EFF_KM`] to the
//! bit. Note the `h` here is the *spherical* height and not [`height_km`]'s
//! quadratic: that is what makes this and [`slant_range_for_ground_km`] an
//! exactly invertible pair rather than merely a close one.
//!
//! It was the tangent-plane projection `r·cos e` until this commit — the
//! small-angle limit of that arc, with the curvature term dropped. The tangent
//! plane erred **outward**, placing every echo further from the radar than the
//! ground it fell on. Measured at each tilt's own reach rather than at an
//! arithmetic ceiling, in metres and in cells of the plan view's
//! `IMAGE_SIZE/(2·BASE_EXTENT_KM)` = 4.4522 px/km (224.61 m to a cell):
//!
//! | tilt | reach | outward error | cells |
//! |---|---:|---:|---:|
//! | 0.5°  | 460.125 km | 666 m  | 2.97 |
//! | 1.8°  | 460.125 km | 1227 m | 5.46 |
//! | 19.5° | 70 km      | 182 m  | 0.81 |
//!
//! It passed one whole cell at 304 km on the 0.5° cut and at 219 km on the
//! 1.8°, so the outer third of one surveillance sweep and the outer half of the
//! other were drawn a visible pixel or more from the ground they sat over.
//!
//! A previous version of this paragraph called that a consistency choice, on
//! the grounds that it was "the same order as the beam-height residual" and ran
//! one way for every consumer. Both halves were wrong. The first quoted 110 m
//! at 230 km / 0.5°, which is arithmetic evaluated half way along a cut that
//! reaches twice as far — at the reach it is six times that. The second holds
//! only while every consumer shares the spelling, which they do not (below).
//!
//! Against outside implementations this now **agrees** rather than costing
//! anything. Py-ART's `antenna_to_cartesian` computes this same arc over this
//! same 4/3 sphere, and the `+proj=aeqd` regrids and wradlib take it too, so
//! the coherent scatter-free per-gate offset a bin-for-bin comparison used to
//! show is gone. What is left between us and Py-ART is that it reads its `s` off
//! as planar azimuthal-equidistant coordinates where this crate walks it as a
//! true arc on [`rustdar_geo::EARTH_RADIUS_KM`] — the split described above,
//! and the one `polar_to_geo` makes as well.
//!
//! # Nothing spells the tangent plane any more
//!
//! For two commits it did, and the table above was the error a viewer was
//! still looking at rather than one that had been fixed: `render`'s four
//! per-tilt paths and [`crate::volumetric::RangeBinning`] each hoisted `cos e`
//! of their sweep's median elevation through their gate loop, so this pair had
//! no caller outside tests and the plan view went on placing gates on the
//! plane. Both now walk the arc per gate, `render::gate_ground_edges` and
//! `RangeBinning::range_of`, and the readout inverts the same arc through
//! `render::polar::PolarGeometry::pick`.
//!
//! The scalar could not simply be swapped for a factor — an arc is not one —
//! so it became a per-gate call, and that was measured before it was accepted
//! rather than argued about: +2.4 % native, +3.1 % in Chrome, nothing
//! measurable in Firefox, on a path that runs once per sweep in a worker and
//! never per animation frame. The worst burst in the system is a 14-frame
//! browser loop build at +0.28 s on ~9.1 s.
//!
//! What that buys is that a section sampled through
//! [`slant_range_for_ground_km`] and the plan view drawn above it are back in
//! register — this time because both are right, not because both were wrong
//! together, which is the failure this module was created to end. The
//! 6371-vs-6378 inconsistency [`crate::types::ImageBounds`] used to carry is a
//! tenth of a pixel beside what was closed here.

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
/// cross-section needs one of these per output row. That is a cost this form
/// meets, not a reason to prefer it: the spherical height inverts in closed
/// form as well (module doc), so what keeps the quadratic is the bit-exact
/// digest pins on [`height_km`], not invertibility.
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

/// Beam-centre height above the radar, km, on the **exact** spherical model —
/// `√(r² + Rₑ² + 2·r·Rₑ·sin e) − Rₑ`, taking the elevation in radians.
///
/// Private, and used only by [`ground_range_km`], which needs the height under
/// the arc. It is deliberately not [`height_km`]: the arc and its inverse
/// [`slant_range_for_ground_km`] are an algebraic pair derived from the same
/// triangle, and feeding the arc a quadratic height would leave the round trip
/// short by the quadratic's own residual instead of by rounding.
///
/// So this crate carries two height models on purpose. [`height_km`] is the
/// public, quadratic, digest-pinned one every drawn product shares; this is an
/// internal detail of a horizontal conversion. The module doc measures the gap
/// between them (23.49 m below 20 km of altitude) — it is not a disagreement
/// about where a beam is, because nothing reads this one's output as a height.
#[inline]
fn spherical_height_km(slant_range_km: f64, elev_rad: f64) -> f64 {
    let r = slant_range_km;
    (r * r + RE_EFF_KM * RE_EFF_KM + 2.0 * r * RE_EFF_KM * elev_rad.sin()).sqrt() - RE_EFF_KM
}

/// Ground range, km: the arc along the earth from the site to the point under
/// a gate at `slant_range_km` on a tilt of `elev_deg`.
///
/// `Rₑ·asin(r·cos e/(Rₑ + h))` with `h` from [`spherical_height_km`] — the
/// exact great-circle arc on the effective sphere, derived in the module doc
/// along with the table of what the tangent-plane `r·cos e` this replaced was
/// worth (666 m outward at the 0.5° cut's 460.125 km reach, 2.97 plan-view
/// cells).
///
/// The shortening against the slant range is 0.5178 km at 230 km / 2.4° and
/// 4.1974 km at 70 km / 19.5°; at the 60° a TDWR's VCP 80 climbs to, an 88.8 km
/// Doppler gate sits over 44.00 km of ground, a shortening of 44.80 km. Those
/// are ranges at a named tilt *and* range, not the pure `cos e` percentages an
/// earlier version of this doc quoted, because the arc is not a per-tilt scale
/// factor — which is the whole reason `render` cannot hoist it.
///
/// The `asin` argument is `sin` of the arc's central angle by construction and
/// cannot round past 1: it is bounded by `r/√(r² + Rₑ²)`, which is 0.054 at the
/// 460 km a WSR-88D reaches and would need `r` of Rₑ's order to approach 1. No
/// clamp, therefore, and no `NaN` from this that was not already in the input.
#[inline]
pub fn ground_range_km(slant_range_km: f64, elev_deg: f64) -> f64 {
    let el = elev_deg.to_radians();
    let h = spherical_height_km(slant_range_km, el);
    RE_EFF_KM * (slant_range_km * el.cos() / (RE_EFF_KM + h)).asin()
}

/// The slant range whose gate sits over `ground_range_km` — the exact inverse
/// of [`ground_range_km`].
///
/// `Rₑ·sin θ/cos(e + θ)` with `θ = s/Rₑ` the arc's central angle. Both come
/// from the triangle on the earth's centre, the radar and the gate: two sides
/// `Rₑ` and `Rₑ + h`, the angle `θ` between them, and `90° + e` at the radar.
/// The law of sines gives this and the arc, the law of cosines gives the
/// height, and `a_ground_range_round_trip_is_exact_on_both_networks` pins the
/// pair at 1.71e-13 km over 235 944 points — the WSR-88D's ladder to 460.1 km
/// and a TDWR's to 88.8, each at its own beam bottom, centre and top.
///
/// **It diverges where `cos(e + θ) = 0`, not at 90°.** A vertical beam covering
/// no ground is the old tangent-plane form's singularity and this one does not
/// have it: at `e = 90°` this returns `−Rₑ` rather than blowing up, which is
/// the arc running away over the far side of the sphere. The real condition is
/// the gate passing the effective sphere's horizon as seen from the radar,
/// `θ = 90° − e`, which is 10 452 km of ground arc at 19.5° and 4448 km at the
/// 60° of a TDWR's VCP 80. Both denominators are ground arcs, matching this
/// function's argument: 4448 km against the 44.00 km of ground a TDWR's 88.8 km
/// Doppler reach covers at 60° is a margin of 101×, so the tilt that was
/// supposed to be the worrying one is not close.
#[inline]
pub fn slant_range_for_ground_km(ground_range_km: f64, elev_deg: f64) -> f64 {
    let el = elev_deg.to_radians();
    let theta = ground_range_km / RE_EFF_KM;
    RE_EFF_KM * theta.sin() / (el + theta).cos()
}

/// Beam-centre height above the radar, km, over a point at `ground_range_km`
/// from the site on a tilt of `elev_deg`.
///
/// [`height_km`] composed with [`slant_range_for_ground_km`], written as that
/// composition. It used to be the folded closed form
/// `s·tan e + s²/(2·Rₑ·cos²e)`, which is the same composition against the
/// *tangent-plane* inverse with the division cancelled; once the inverse became
/// the spherical arc there is no such cancellation to make and the folded form
/// would be a third, unrelated answer.
///
/// Writing it as the composition keeps two properties that the folded form used
/// to have to be measured for.
/// `the_ground_range_height_is_the_slant_range_height_over_the_same_point` now
/// holds **by construction** and to the bit, rather than to the 2.8e-14 km the
/// two spellings used to differ by. And the height model stays the quadratic
/// one — this is not a route by which the spherical height reaches a drawn
/// product.
///
/// Still closed form, and still no iteration: one sine and two cosines for the
/// inverse and a multiply-add for the height. A cross-section evaluating it per
/// output column pays what it did before, so nothing about that use changes.
#[inline]
pub fn height_at_ground_km(ground_range_km: f64, elev_deg: f64) -> f64 {
    height_km(
        slant_range_for_ground_km(ground_range_km, elev_deg),
        elev_deg,
    )
}

#[cfg(test)]
mod tests;
