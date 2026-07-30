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
//! # Horizontal geometry: 6371, tangent plane
//!
//! [`site_bearing_range_km`] and [`great_circle_point`] measure on a sphere of
//! [`crate::types::EARTH_RADIUS_KM`] (6371 km) — deliberately the same
//! constant [`crate::render`]'s `render_gate` projects gates with, so a line
//! drawn on a plan view lands on the ground the plan view put under the
//! cursor. It is **not** the `1.0 / 111.32` degrees-per-km that
//! [`crate::types::ImageBounds`] implies, which is a 6378 km sphere: that is a
//! known 0.11 % inconsistency in the image bounds, and reproducing it here
//! would spread it instead of containing it. The map's hover readout reads
//! [`site_bearing_range_km`] for exactly that reason — it is the range and
//! azimuth of the ground the plan view put under the cursor, so it has to be
//! measured the way the plan view placed it.
//!
//! [`ground_range_km`] is the tangent-plane projection `r·cos e`, matching
//! `render_gate`'s own `r·sin az` / `r·cos az`, and not the spherical arc
//! `Rₑ·asin(r·cos e/(Rₑ + h))` that `polar_to_geo` returns. Those differ by
//! ~110 m at 230 km / 0.5° and ~182 m at 70 km / 19.5° — the same order as the
//! beam-height residual, and in the same direction for every consumer, which
//! is what makes it a consistency choice rather than an accuracy claim. Note
//! `render_gate` applies **no** `cos e` at all (it never receives an
//! elevation angle), so a consumer that does apply it will not register
//! against the plan view above ~2°. That divergence is real, deliberate, and
//! belongs to the consumer to declare — this module only supplies the
//! `cos e`.

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
pub const HALF_POWER_BEAMWIDTH_DEG: f64 = 0.95;

/// Beam-centre height above the radar, km, at a slant range and elevation.
///
/// The vertical coordinate every drawn product in this crate shares. Heights
/// are **above the antenna**, not above MSL; a caller wanting MSL adds the
/// site height itself (see [`crate::eet::radar_height_ft_near`]).
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
/// Tangent-plane `r·cos e`, per the module doc. This is the factor
/// `render_gate` omits: it is 0.09 % at 2.4° and 5.7 % at 19.5°, i.e. 0.2 km
/// and 4.0 km at those tilts' plotted extents.
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
    // on 1.0 and misses all 680 that did not, and only the `hav` test below
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
mod tests {
    use super::*;

    /// The exact spherical height on the same effective sphere — the form
    /// `nexrad_model`'s `polar_to_geo` uses. Written out here rather than
    /// called through `polar_to_geo` because that function also applies an
    /// antenna height and converts to geography; the residual under test is
    /// the height model alone.
    fn spherical_height_km(slant_range_km: f64, elev_deg: f64) -> f64 {
        let r = slant_range_km;
        let re = RE_EFF_KM;
        (r * r + re * re + 2.0 * r * re * elev_deg.to_radians().sin()).sqrt() - re
    }

    /// The tilt ladder of VCP 212, the densest operational pattern, plus the
    /// endpoints of the domain the crate plots.
    const ELEVS: [f64; 16] = [
        0.2, 0.5, 0.9, 1.3, 1.8, 2.4, 3.1, 4.0, 5.1, 6.4, 8.0, 10.0, 12.0, 14.0, 16.7, 19.5,
    ];

    /// The one constant two modules used to carry privately, pinned against
    /// both of their spellings.
    ///
    /// Float multiplication does not associate in general, so
    /// `volumetric`'s `6371.0 * 4.0 / 3.0` and `nrot`'s `4.0 / 3.0 * 6371.0`
    /// were not *guaranteed* to be the same `f64` — they happen to be, which
    /// is what makes deleting `nrot`'s copy a pure de-duplication and not a
    /// silent shift in every NROT wind-profile layer assignment. If a future
    /// edit reaches for a rounded literal, this fails before the five
    /// golden-digest assertions do, and says why.
    #[test]
    fn the_shared_effective_earth_radius_is_bit_identical_to_both_deleted_copies() {
        let volumetric_spelling: f64 = 6371.0 * 4.0 / 3.0;
        let nrot_spelling: f64 = 4.0 / 3.0 * 6371.0;
        assert_eq!(
            RE_EFF_KM.to_bits(),
            volumetric_spelling.to_bits(),
            "RE_EFF_KM {RE_EFF_KM} != volumetric's {volumetric_spelling}",
        );
        assert_eq!(
            RE_EFF_KM.to_bits(),
            nrot_spelling.to_bits(),
            "RE_EFF_KM {RE_EFF_KM} != nrot's {nrot_spelling}",
        );
        assert_eq!(
            RE_EFF_KM.to_bits(),
            (4.0 / 3.0 * EARTH_RADIUS_KM).to_bits(),
            "the effective radius is no longer 4/3 of the crate's mean radius",
        );
        // precondition: a rounded literal is a *different* f64, so the
        // assertions above are load-bearing rather than trivially true.
        assert_ne!(
            RE_EFF_KM.to_bits(),
            8494.667f64.to_bits(),
            "precondition: 8494.667 rounds to the same bits as the exact \
             expression, so this module's headline warning is vacuous",
        );
    }

    /// The inverse is exact, not fitted: every height the forward model
    /// produces maps back to the slant range that produced it.
    ///
    /// Over the whole plotted domain — 1..460 km (twice `MAX_RANGE_KM`,
    /// because reflectivity moments really do run past 300 km) × the VCP 212
    /// ladder. 1e-9 km is 1 µm; the assertion is that the algebra is right,
    /// not that it is close.
    #[test]
    fn the_beam_height_inverse_returns_the_range_the_height_came_from() {
        let mut checked = 0usize;
        let mut worst = (0.0f64, 0.0f64, 0.0f64); // (err, r, e)
        for &e in &ELEVS {
            for r in 1..=460 {
                let r = f64::from(r);
                let h = height_km(r, e);
                let back = slant_range_for_height_km(h, e);
                let err = (back - r).abs();
                if err > worst.0 {
                    worst = (err, r, e);
                }
                checked += 1;
            }
        }
        // precondition: the table really covered the domain, so a round trip
        // that silently degenerated to a handful of points cannot pass.
        assert_eq!(
            checked,
            ELEVS.len() * 460,
            "precondition: the round-trip table did not cover 460 ranges × \
             {} elevations",
            ELEVS.len(),
        );
        assert!(
            worst.0 < 1e-9,
            "the inverse is not exact: worst round-trip error {:e} km at \
             {} km / {}°",
            worst.0,
            worst.1,
            worst.2,
        );

        // The domain edge the doc names: nothing ascends below −Rₑ·sin²e/2, and
        // the inverse says so with `NaN` rather than with a plausible range.
        // −0.32 km at 0.5°, zero at 0°.
        let floor = |e: f64| -RE_EFF_KM * e.to_radians().sin().powi(2) / 2.0;
        assert!(
            (floor(0.5) + 0.323_445).abs() < 1e-6,
            "the 0.5° descent floor moved: {} km",
            floor(0.5),
        );
        assert!(
            slant_range_for_height_km(floor(0.5) - 0.001, 0.5).is_nan(),
            "a height below the 0.5° beam's floor returned a range",
        );
        assert!(
            slant_range_for_height_km(-0.001, 0.0).is_nan(),
            "a height below a 0° beam returned a range",
        );
        // And just inside it still resolves, so the guard is a boundary and not
        // a wholesale refusal of low heights.
        assert!(
            slant_range_for_height_km(floor(0.5) + 0.001, 0.5).is_finite(),
            "a height just inside the 0.5° floor was refused",
        );
        assert_eq!(slant_range_for_height_km(0.0, 0.5), 0.0);
    }

    /// The quadratic against the exact spherical form at the corners of the
    /// domain, with the numbers the module doc quotes.
    ///
    /// Pins both halves of the claim: that the approximation is negligible
    /// where products are drawn, *and* that it is not negligible at
    /// 230 km / 19.5°. The second assertion is the one that matters — a
    /// future edit that widened the domain and left the doc's "under one
    /// gate" claim standing would be caught here.
    #[test]
    fn the_quadratic_beam_height_tracks_the_spherical_form_within_the_measured_residual() {
        let resid_m = |r: f64, e: f64| (height_km(r, e) - spherical_height_km(r, e)) * 1000.0;

        // The two figures the doc quotes, to 0.01 m.
        let far_low = resid_m(230.0, 0.5);
        assert!(
            (far_low - 1.54).abs() < 0.01,
            "230 km / 0.5° residual moved: {far_low:.3} m, expected 1.54 m",
        );
        let near_high = resid_m(70.0, 19.5);
        assert!(
            (near_high - 32.84).abs() < 0.01,
            "70 km / 19.5° residual moved: {near_high:.3} m, expected 32.84 m",
        );

        // Both are far under one 250 m gate, which is the claim they support.
        assert!(
            far_low < 250.0 && near_high < 250.0,
            "a plotted-domain corner now exceeds one 250 m gate: \
             {far_low:.1} m at 230 km/0.5°, {near_high:.1} m at 70 km/19.5°",
        );

        // And the corner that does not: 230 km / 19.5° is ~372 m, larger than
        // a gate, at a beam height of ~79.9 km.
        let far_high = resid_m(230.0, 19.5);
        assert!(
            (far_high - 372.17).abs() < 0.01,
            "230 km / 19.5° residual moved: {far_high:.3} m, expected 372.17 m",
        );
        assert!(
            far_high > 250.0,
            "precondition: 230 km/19.5° no longer exceeds one 250 m gate, so \
             the module's domain-dependence caveat has nothing to warn about \
             ({far_high:.1} m at a beam height of {:.1} km)",
            height_km(230.0, 19.5),
        );

        // The ceiling the caveat is actually stated in: nothing below 20 km
        // of altitude, at any elevation, exceeds 23.6 m. The measured worst case
        // is 23.489 m — bounded at 23.6 rather than 23.5 so the assertion is not
        // a hairline over a figure the doc quotes to two decimals.
        for &e in &ELEVS {
            let r = slant_range_for_height_km(20.0, e);
            let at_ceiling = resid_m(r, e);
            assert!(
                at_ceiling < 23.6,
                "the 20 km altitude ceiling no longer bounds the residual at \
                 23.6 m: {at_ceiling:.3} m at {r:.1} km / {e}° (height {:.3} km)",
                height_km(r, e),
            );
        }
        // The figure the module doc states, to the precision it states it.
        let at_20km = resid_m(slant_range_for_height_km(20.0, 19.5), 19.5);
        assert!(
            (at_20km - 23.489).abs() < 0.001,
            "the 20 km ceiling residual moved: {at_20km:.4} m, documented as \
             23.489 m",
        );
    }

    /// Why the caveat is stated as a height and not a range: the residual is
    /// a function of the quadratic height *alone*.
    ///
    /// `h_quad − h_sphere = Rₑ·((1+q) − √(1+2q))` with `q = h_quad/Rₑ`, which
    /// holds exactly because `r² + Rₑ² + 2rRₑ·sin e ≡ Rₑ² + 2Rₑ·h_quad`.
    /// Checked across ranges and elevations that produce the same height by
    /// different routes, so a residual that secretly depended on `r` or `e`
    /// would separate them.
    ///
    /// The tolerance bounds the **measurement**, not the identity, and is
    /// derived rather than tuned: `spherical_height_km` subtracts Rₑ from a
    /// square root a few km larger, so its absolute error floor is `ε·Rₑ`
    /// ≈ 1.9e-12 km however exact the algebra is. The measured worst case over
    /// this grid is 2.575e-12 km — 1.4× that floor — and the tolerance
    /// (`4·ε·Rₑ` = 7.5e-12 km) leaves ~3× headroom above it. The residuals
    /// themselves run 5.885e-5 km (h = 1 km) to 1.463e-1 km (h = 50 km), so even
    /// the *smallest* of them stands seven orders of magnitude above the
    /// tolerance and a genuine break in the identity has nowhere to hide.
    #[test]
    fn the_beam_height_residual_depends_only_on_the_height() {
        // `(1+q)² − q²` is identically `1 + 2q`; the shorter form is used both
        // here and in the module doc, and drops a squaring.
        let from_height = |h: f64| {
            let q = h / RE_EFF_KM;
            RE_EFF_KM * ((1.0 + q) - (1.0 + 2.0 * q).sqrt())
        };
        let cancellation_floor = f64::EPSILON * RE_EFF_KM;
        let tolerance = 4.0 * cancellation_floor;
        let mut pairs = 0usize;
        let mut worst = 0.0f64;
        for &e in &ELEVS {
            for h in [1.0, 3.0, 8.0, 15.0, 20.0, 30.0, 50.0] {
                let r = slant_range_for_height_km(h, e);
                let measured = height_km(r, e) - spherical_height_km(r, e);
                let predicted = from_height(height_km(r, e));
                let err = (measured - predicted).abs();
                worst = worst.max(err);
                assert!(
                    err < tolerance,
                    "the height-only identity broke at {r:.3} km / {e}° \
                     (h {h} km): measured {measured:e} km, predicted \
                     {predicted:e} km, {:.1}× the {cancellation_floor:e} km \
                     cancellation floor",
                    err / cancellation_floor,
                );
                pairs += 1;
            }
        }
        // precondition: several elevations really did reach each height, which
        // is the only reason "depends only on height" is a claim at all.
        assert_eq!(pairs, ELEVS.len() * 7, "precondition: the grid shrank");
        // precondition: the tolerance is the float floor and not slack — if
        // the grid ever sat far below it, the assertion above would stop
        // discriminating and should be tightened.
        assert!(
            worst > cancellation_floor,
            "precondition: the worst identity error {worst:e} km is now below \
             the {cancellation_floor:e} km cancellation floor, so the \
             tolerance is slack rather than derived",
        );

        // The algebra the whole claim rests on, asserted directly rather than
        // only through its consequence: the spherical form's radicand *is*
        // `Rₑ² + 2·Rₑ·h_quad`. Exact in algebra; in f64 the right-hand side
        // re-rounds one multiply, so 20 of these 7360 pairs differ by a single
        // ulp. Bounded at 2 ulps of relative error, which is a statement about
        // rounding and not about the identity.
        let mut radicand_pairs = 0usize;
        for &e in &ELEVS {
            for ri in 1..=460 {
                let r = f64::from(ri);
                let direct =
                    r * r + RE_EFF_KM * RE_EFF_KM + 2.0 * r * RE_EFF_KM * e.to_radians().sin();
                let via_height = RE_EFF_KM * RE_EFF_KM + 2.0 * RE_EFF_KM * height_km(r, e);
                assert!(
                    (direct - via_height).abs() / direct < 2.0 * f64::EPSILON,
                    "the radicand identity broke at {r} km / {e}°: {direct} vs \
                     {via_height} ({:.2} ulps)",
                    (direct - via_height).abs() / direct / f64::EPSILON,
                );
                radicand_pairs += 1;
            }
        }
        assert_eq!(radicand_pairs, ELEVS.len() * 460);
    }

    /// The move out of `volumetric` changed no bit of arithmetic.
    ///
    /// Recomputes the expression `volumetric::beam_height_km` shipped —
    /// transcribed here, not called — and demands bit-identity over the
    /// domain the echo-tops cube evaluates it on (1-km cell centres × the
    /// tilt ladder, plus the ±half-beamwidth offsets `BeamHeights` uses).
    /// This is the fast, local guard for the same property the five pinned
    /// `0x4559ce366731e030` digests guard end to end.
    #[test]
    fn the_lifted_beam_height_is_bit_identical_to_the_one_volumetric_shipped() {
        // Verbatim from the pre-lift `volumetric::beam_height_km`, including
        // its private `RE_EFF_KM` spelling.
        fn shipped(range_km: f64, elev_deg: f64) -> f64 {
            const RE_EFF_KM: f64 = 6371.0 * 4.0 / 3.0;
            let el = elev_deg.to_radians();
            range_km * el.sin() + range_km * range_km / (2.0 * RE_EFF_KM)
        }

        let half = HALF_POWER_BEAMWIDTH_DEG / 2.0;
        let mut checked = 0usize;
        for &e in &ELEVS {
            for e in [e - half, e, e + half] {
                // `RANGE_BINS` cell centres, the grid `BeamHeights` builds.
                for cell in 0..crate::volumetric::RANGE_BINS {
                    let r = cell as f64 + 0.5;
                    assert_eq!(
                        height_km(r, e).to_bits(),
                        shipped(r, e).to_bits(),
                        "beam height moved at {r} km / {e}°: {} vs {}",
                        height_km(r, e),
                        shipped(r, e),
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(
            checked,
            ELEVS.len() * 3 * crate::volumetric::RANGE_BINS,
            "precondition: the bit-identity grid did not cover every tilt × \
             beam edge × range cell",
        );
    }

    /// Ground range and its inverse round-trip, and the closed-form height
    /// over a ground range agrees with going through the slant range.
    #[test]
    fn the_ground_range_height_is_the_slant_range_height_over_the_same_point() {
        let mut worst_trip = 0.0f64;
        let mut worst_height = (0.0f64, 0.0f64, 0.0f64);
        for &e in &ELEVS {
            for r in 1..=460 {
                let r = f64::from(r);
                let g = ground_range_km(r, e);
                worst_trip = worst_trip.max((slant_range_for_ground_km(g, e) - r).abs());

                let folded = height_at_ground_km(g, e);
                let composed = height_km(slant_range_for_ground_km(g, e), e);
                let err = (folded - composed).abs();
                if err > worst_height.0 {
                    worst_height = (err, r, e);
                }
            }
        }
        // Both bounds are ~1.5 orders above the measured worst case (5.7e-14
        // and 2.8e-14 km) and four below a slack 1e-9, so a reassociation that
        // really changed the arithmetic has nowhere to hide.
        assert!(
            worst_trip < 1e-12,
            "the ground-range round trip is not exact: worst error {worst_trip:e} km",
        );
        assert!(
            worst_height.0 < 1e-12,
            "the folded and composed height forms disagree by {:e} km at \
             {} km / {}° — more than the 2.8e-14 km the doc measures",
            worst_height.0,
            worst_height.1,
            worst_height.2,
        );

        // The `cos e` the plan view omits, at the two tilts the divergence is
        // quoted at: 0.2 km at 2.4° and 4.0 km at 19.5°.
        let at_low = 230.0 - ground_range_km(230.0, 2.4);
        let at_high = 70.0 - ground_range_km(70.0, 19.5);
        assert!(
            (at_low - 0.2017).abs() < 1e-3,
            "the 2.4° slant/ground gap moved: {at_low:.4} km, expected 0.2017",
        );
        assert!(
            (at_high - 4.0151).abs() < 1e-3,
            "the 19.5° slant/ground gap moved: {at_high:.4} km, expected 4.0151",
        );
    }

    /// Bearing and range against hand-checkable geometry: due north, due east,
    /// and a site to itself.
    #[test]
    fn the_site_bearing_and_range_agree_with_hand_computed_geometry() {
        // KTLX, near enough. One degree of latitude due north is
        // Re·(π/180) = 111.1949 km on a 6371 km sphere.
        let (site_lat, site_lon) = (35.3333, -97.2778);
        let (bearing, range) = site_bearing_range_km(site_lat, site_lon, site_lat + 1.0, site_lon);
        assert!(
            (range - EARTH_RADIUS_KM * std::f64::consts::PI / 180.0).abs() < 1e-6,
            "a degree due north measured {range} km",
        );
        assert!(bearing.abs() < 1e-9, "due north read as {bearing}°");

        // A degree due east is where a flat approximation and a great circle
        // separate, and both ways round.
        //
        // The initial bearing is *not* 90°: the great circle between two points
        // at one latitude bows poleward, so it leaves north of east by
        // ≈ (Δλ/2)·sin φ = 0.2892°, and 90° would mean this function had
        // silently become a rhumb-line bearing.
        let (bearing, range) = site_bearing_range_km(site_lat, site_lon, site_lat, site_lon + 1.0);
        let poleward_bow = 0.5 * site_lat.to_radians().sin();
        assert!(
            (bearing - (90.0 - poleward_bow)).abs() < 1e-4,
            "due east read as {bearing}°, expected ~{}° (90° less the \
             {poleward_bow:.4}° poleward bow)",
            90.0 - poleward_bow,
        );

        // And the distance is *shorter* than the parallel of latitude it looks
        // like it follows — 0.385 m over one degree at this site. Small, but
        // the sign is the tell: a flat `Δλ·cos φ` cannot be shorter.
        let parallel = EARTH_RADIUS_KM * std::f64::consts::PI / 180.0 * site_lat.to_radians().cos();
        let chord_saving_m = (parallel - range) * 1000.0;
        assert!(
            (chord_saving_m - 0.385).abs() < 0.01,
            "a degree due east measured {range} km against the {parallel} km \
             parallel arc — a {chord_saving_m:.3} m saving, expected 0.385 m",
        );

        // Degenerate: a site to itself is zero range. The bearing is
        // unconstrained, so it is not asserted.
        let (_, range) = site_bearing_range_km(site_lat, site_lon, site_lat, site_lon);
        assert_eq!(range, 0.0, "a site is not at zero range from itself");

        // precondition: `EARTH_RADIUS_KM` is the 6371 sphere, not the 6378 one
        // `ImageBounds` implies — the whole point of the module doc's note.
        assert_eq!(EARTH_RADIUS_KM, 6371.0);
        assert!(
            (EARTH_RADIUS_KM * std::f64::consts::PI / 180.0 - 111.32).abs() > 0.1,
            "precondition: the 6371 sphere and `ImageBounds`' implied 111.32 \
             km/° have converged, so recording the seam is pointless",
        );
    }

    /// The hover readout's own haversine and forward azimuth, deleted from
    /// `ui_map::compute_hover_info_raw` in favour of this module, pinned against
    /// the spelling that replaced it.
    ///
    /// Float multiplication does not associate, and the two spellings do not
    /// group the radius the same way — the copy computed `Rₑ · (2 · atan2(..))`
    /// and this module computes `(Rₑ · 2) · atan2(..)`. They agree because the
    /// factor is 2, which scales exactly, but that is a fact about this
    /// expression and not a general licence: the same de-duplication with a
    /// factor of 3 in it would have moved every range the readout has ever
    /// printed. This is what says the digits did not move.
    ///
    /// The two are *not* identical everywhere, and the exception is measured
    /// rather than waved past. The copy had no counterpart to the `clamp`, so at
    /// a point antipodal to the site to within rounding it produced a `NaN`
    /// range where this produces the half-circumference. The window is about
    /// 5e-13 degrees wide — nine orders of magnitude below the ~3e-7 degrees one
    /// screen pixel spans at the deepest zoom the map offers, and the hover
    /// coordinates are unprojected from an integer pixel — so no cursor can
    /// address it, and the readout it replaces is `NaN` rather than a number.
    #[test]
    fn the_hover_readouts_polar_coordinates_are_bit_identical_to_the_deleted_copy() {
        /// Transcribed character-for-character from the deleted block, including
        /// the association of the radius and the absence of a clamp. Do not
        /// "tidy" it: its value is that it is literally what shipped.
        fn deleted_copy(
            site_lat: f64,
            site_lon: f64,
            hover_lat: f64,
            hover_lon: f64,
        ) -> (f64, f64) {
            let lat1 = site_lat.to_radians();
            let lon1 = site_lon.to_radians();
            let lat2 = hover_lat.to_radians();
            let lon2 = hover_lon.to_radians();
            let dlat = lat2 - lat1;
            let dlon = lon2 - lon1;
            let a =
                (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
            let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
            let distance_km = EARTH_RADIUS_KM * c;

            let y = dlon.sin() * lat2.cos();
            let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * dlon.cos();
            let azimuth = (y.atan2(x).to_degrees() + 360.0) % 360.0;
            (azimuth, distance_km)
        }

        // Nine sites spanning the network rather than one: the azimuth's `x`
        // term carries `sin φ`, so a single mid-latitude site would leave the
        // equatorial and sub-polar ends of the network untested, and PGUA is
        // the only one east of the antimeridian.
        let sites = [
            ("KTLX", 35.3333, -97.2778),
            ("KMPX", 44.8489, -93.5656),
            ("KBGM", 42.1997, -75.9847),
            ("KEWX", 29.7039, -98.0283),
            ("KATX", 48.1945, -122.4958),
            ("PABC", 60.7919, -161.8763),
            ("PHKI", 21.8938, -159.5522),
            ("TJUA", 18.1156, -66.0781),
            ("PGUA", 13.4556, 144.8111),
        ];

        let mut checked = 0_u32;
        for (name, site_lat, site_lon) in sites {
            // Out to ±6°, which is past the 460 km the widest plan view draws,
            // in steps small enough to land between the grid nodes.
            for i in -30..=30 {
                for j in -30..=30 {
                    let hover_lat = site_lat + f64::from(i) * 0.2137;
                    let hover_lon = site_lon + f64::from(j) * 0.2137;
                    let (az_old, range_old) =
                        deleted_copy(site_lat, site_lon, hover_lat, hover_lon);
                    let (az_new, range_new) =
                        site_bearing_range_km(site_lat, site_lon, hover_lat, hover_lon);
                    assert_eq!(
                        range_old.to_bits(),
                        range_new.to_bits(),
                        "{name} -> ({hover_lat}, {hover_lon}): range {range_old} became {range_new}",
                    );
                    assert_eq!(
                        az_old.to_bits(),
                        az_new.to_bits(),
                        "{name} -> ({hover_lat}, {hover_lon}): azimuth {az_old} became {az_new}",
                    );
                    checked += 1;
                }
            }
            // And the far field a panned-out map reaches, the site's own
            // antipode included — the exact antipode agrees, so the divergence
            // below really is confined to the rounding around it.
            for lat in [-89.9, -45.0, 0.0, 45.0, 89.9, -site_lat] {
                for lon in [-179.9, -90.0, 0.0, 90.0, 179.9, site_lon + 180.0] {
                    let (az_old, range_old) = deleted_copy(site_lat, site_lon, lat, lon);
                    let (az_new, range_new) = site_bearing_range_km(site_lat, site_lon, lat, lon);
                    assert_eq!(
                        range_old.to_bits(),
                        range_new.to_bits(),
                        "{name} -> ({lat}, {lon}): range {range_old} became {range_new}",
                    );
                    assert_eq!(az_old.to_bits(), az_new.to_bits());
                    checked += 1;
                }
            }
        }
        // precondition: the grid ran at full size. Exact rather than a floor, so
        // a loop bound quietly narrowed to one site or one ring fails here
        // instead of leaving a weaker test passing.
        assert_eq!(checked, 33_813, "the comparison grid changed size");

        // The one divergence, in the direction that only ever replaces a
        // non-number. Two ulps off KTLX's antipodal latitude is enough to round
        // the haversine over 1.0.
        let (site_lat, site_lon) = (35.3333, -97.2778);
        let (near_antipodal_lat, antipodal_lon) = (-35.33329999999999, 82.7222);
        let (_, range_old) = deleted_copy(site_lat, site_lon, near_antipodal_lat, antipodal_lon);
        let (_, range_new) =
            site_bearing_range_km(site_lat, site_lon, near_antipodal_lat, antipodal_lon);
        assert!(
            range_old.is_nan(),
            "precondition: the deleted copy answered {range_old} here, so the \
             clamp is what this pair is testing",
        );
        assert!(
            (range_new - EARTH_RADIUS_KM * std::f64::consts::PI).abs() < 1e-6,
            "the clamp should give the half-circumference, gave {range_new}",
        );
    }

    /// The interpolation's endpoints, its degenerate cases, and the property a
    /// cross-section's column loop depends on: `t` is a fraction of ground
    /// range, exactly.
    #[test]
    fn a_fraction_along_a_line_is_that_fraction_of_its_ground_range() {
        let a = (35.3333, -97.2778); // KTLX
        let b = (36.1750, -95.5644); // near KINX, ~170 km ENE

        let at_zero = great_circle_point(a, b, 0.0);
        assert!(
            (at_zero.0 - a.0).abs() < 1e-9 && (at_zero.1 - a.1).abs() < 1e-9,
            "t=0 landed at {at_zero:?}, not {a:?}",
        );
        let at_one = great_circle_point(a, b, 1.0);
        assert!(
            (at_one.0 - b.0).abs() < 1e-9 && (at_one.1 - b.1).abs() < 1e-9,
            "t=1 landed at {at_one:?}, not {b:?}",
        );

        // The load-bearing property: from a site at `a`, the point at `t` is
        // at `t` of the total ground range. A lat/lon lerp fails this.
        let (bearing_b, total) = site_bearing_range_km(a.0, a.1, b.0, b.1);
        // precondition: the fixture line is long enough for a bent
        // interpolation to show up at all.
        assert!(
            total > 150.0,
            "precondition: the fixture line is only {total:.1} km long",
        );
        for step in 0..=20 {
            let t = f64::from(step) / 20.0;
            let p = great_circle_point(a, b, t);
            let (bearing, range) = site_bearing_range_km(a.0, a.1, p.0, p.1);
            assert!(
                (range - t * total).abs() < 1e-6,
                "t={t} sits at {range:.6} km, not {:.6}",
                t * total,
            );
            // Every interior point is on the same initial bearing out of `a`.
            if step > 0 {
                assert!(
                    (bearing - bearing_b).abs() < 1e-6,
                    "t={t} bears {bearing}° from the site, not {bearing_b}°",
                );
            }
        }

        // A midpoint from a lat/lon lerp is measurably *not* the great-circle
        // midpoint, which is why this function exists.
        let mid = great_circle_point(a, b, 0.5);
        let lerp = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        let (_, sag) = site_bearing_range_km(mid.0, mid.1, lerp.0, lerp.1);
        assert!(
            sag > 0.005,
            "precondition: over a {total:.0} km line the lat/lon lerp is only \
             {:.1} m off the great circle, so this test proves nothing",
            sag * 1000.0,
        );

        // Extrapolation past the endpoints stays on the circle.
        let beyond = great_circle_point(a, b, 1.5);
        let (_, range) = site_bearing_range_km(a.0, a.1, beyond.0, beyond.1);
        assert!(
            (range - 1.5 * total).abs() < 1e-6,
            "t=1.5 sits at {range:.6} km, not {:.6}",
            1.5 * total,
        );
    }

    /// The two degenerate geometries, asserted as the contract states them
    /// rather than as "something finite came back".
    ///
    /// Antipodal is the case worth a test of its own, because two successive
    /// obvious guards do not work and neither fails loudly.
    ///
    /// `sin d == 0.0` catches **none** of 3602 antipodal pairs: `d` is the f64
    /// nearest π, whose sine is 1.2246e-16. Tightening to
    /// `|sin d| < f64::EPSILON` catches 2922 and still misses 680, because a
    /// truly antipodal pair does not reliably land `hav` on exactly 1.0 — 2922
    /// (81.1 %) do, 648 (18.0 %) fall one ulp short and 32 (0.89 %) two, and
    /// `√(1 − hav)` turns even one ulp into `sin d ≈ 2e-8`. Both leakages return
    /// `(0.0, 0.0)` — null island — or `NaN` where the haversine rounded past
    /// 1.0, and neither reads as a failure at the call site. Only the
    /// well-conditioned `hav` test catches all 3602, which is why the guard is
    /// written on `hav`; `the_degeneracy_guard_is_a_derived_threshold`
    /// re-measures all three so the choice cannot be quietly undone.
    #[test]
    fn a_degenerate_pair_of_endpoints_returns_the_first_one() {
        let a = (35.3333, -97.2778);

        // Coincident.
        assert_eq!(
            great_circle_point(a, a, 0.5),
            a,
            "coincident endpoints did not degenerate to `a`",
        );

        // Antipodal, swept: every latitude, at three fractions, in both
        // longitude directions. All must be `a` exactly — not merely finite,
        // and never null island.
        let mut checked = 0usize;
        for tenths in -900..=900 {
            let lat = f64::from(tenths) / 10.0;
            for lon_shift in [180.0, -180.0] {
                let anti = (-lat, -97.2778 + lon_shift);
                for t in [0.0, 0.5, 1.0] {
                    let p = great_circle_point((lat, -97.2778), anti, t);
                    assert_eq!(
                        p,
                        (lat, -97.2778),
                        "antipodal pair ({lat}, -97.2778)/{anti:?} at t={t} \
                         returned {p:?} instead of the first endpoint",
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 1801 * 2 * 3, "precondition: the sweep shrank");

        // And the same pairs give `site_bearing_range_km` the half
        // circumference rather than a `NaN` range — the clamp's own property.
        //
        // The residual error is bounded in closed form, and the bound is the
        // *offset* rather than an amplified rounding. The haversine is
        // ill-conditioned at antipodal and the clamp cannot change that: with
        // `u = 1 − hav`, `d = π − 2√u`, so the range falls short of the half
        // circumference by `Rₑ·2√u` — an error that **grows** with `u` rather
        // than shrinking. `u` is at most `f64::EPSILON` (two ulps below 1.0), so
        // the bound is `2·Rₑ·√ε` = 0.18987 m, attained at the 32 pairs of this
        // sweep that land two ulps low. (An earlier comment here derived
        // `ε/√u` and quoted 0.13 m; that is the error in `d` *inherited from
        // hav's own last ulp*, a second-order effect, and it happens to be the
        // same order only at `u` = one ulp. The offset dominates.)
        //
        // What the clamp buys is that the answer exists at all: without it,
        // 3.7 % of these pairs are `NaN`.
        let half_circumference = EARTH_RADIUS_KM * std::f64::consts::PI;
        let bound_m = 2.0 * EARTH_RADIUS_KM * f64::EPSILON.sqrt() * 1000.0;
        let mut worst_m = 0.0f64;
        for tenths in -900..=900 {
            let lat = f64::from(tenths) / 10.0;
            let (_, range) = site_bearing_range_km(lat, -97.2778, -lat, 82.7222);
            assert!(
                range.is_finite(),
                "an antipodal pair at latitude {lat} measured a non-finite \
                 range ({range}) — the haversine clamp is gone",
            );
            let err_m = (range - half_circumference).abs() * 1000.0;
            worst_m = worst_m.max(err_m);
            assert!(
                err_m <= bound_m * 1.01,
                "an antipodal pair at latitude {lat} measured {range} km, \
                 {err_m:.5} m off the {half_circumference} km half \
                 circumference — past the derived {bound_m:.5} m bound",
            );
        }
        // The bound is attained, not merely respected: it is the closed form and
        // not a tolerance, so the measurement should sit *on* it.
        assert!(
            (worst_m - bound_m).abs() < 1e-9,
            "the worst antipodal range error is {worst_m:.5} m against a derived \
             bound of {bound_m:.5} m — the two should coincide, so either the \
             formula or the derivation moved",
        );
        assert!(
            (worst_m - 0.18987).abs() < 1e-5,
            "the antipodal range error moved: {worst_m:.5} m, documented as \
             0.18987 m",
        );

        // A non-finite input is documented as propagating rather than being
        // caught: `hav` is NaN, which fails both of the guard's comparisons.
        let nan_in = great_circle_point((f64::NAN, 0.0), (1.0, 1.0), 0.5);
        assert!(
            nan_in.0.is_nan() && nan_in.1.is_nan(),
            "a NaN endpoint was silently absorbed, returning {nan_in:?}",
        );
    }

    /// The guard's threshold is derived, and the two thresholds it replaced are
    /// re-measured here so nobody can "simplify" it back.
    ///
    /// `64·f64::EPSILON` comes from the conditioning: with `u = 1 − hav`, the
    /// divisor `sin d ≈ 2√u` while `hav`'s last ulp reaches `d` amplified to
    /// `≈ ε/√u`, so the divisor's relative error is `≈ ε/(2u)` and passes 1 %
    /// below ~50ε. What it costs is 1.519 m of reach — the shortest line the
    /// function will still interpolate — which is 165× finer than one 250 m
    /// gate.
    #[test]
    fn the_degeneracy_guard_is_a_derived_threshold() {
        const MIN_CONDITIONING: f64 = 64.0 * f64::EPSILON;

        // Re-derive `hav` the way the function does, for a truly antipodal pair.
        let hav_of = |lat: f64, lon_shift: f64| {
            let (lat1, lon1) = (lat.to_radians(), (-97.2778f64).to_radians());
            let (lat2, lon2) = ((-lat).to_radians(), (-97.2778 + lon_shift).to_radians());
            let (dlat, dlon) = (lat2 - lat1, lon2 - lon1);
            ((dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2))
                .clamp(0.0, 1.0)
        };

        let mut total = 0usize;
        let mut caught_by_exact_zero = 0usize;
        let mut caught_by_epsilon = 0usize;
        let mut caught_by_conditioning = 0usize;
        for tenths in -900..=900 {
            let lat = f64::from(tenths) / 10.0;
            for lon_shift in [180.0, -180.0] {
                let hav = hav_of(lat, lon_shift);
                let d = 2.0 * hav.sqrt().atan2((1.0 - hav).sqrt());
                let sin_d = d.sin();
                total += 1;
                if sin_d == 0.0 {
                    caught_by_exact_zero += 1;
                }
                if sin_d.abs() < f64::EPSILON {
                    caught_by_epsilon += 1;
                }
                if hav < MIN_CONDITIONING || 1.0 - hav < MIN_CONDITIONING {
                    caught_by_conditioning += 1;
                }
            }
        }
        assert_eq!(total, 3602, "precondition: the antipodal sweep shrank");
        assert_eq!(
            caught_by_exact_zero, 0,
            "`sin d == 0.0` now catches antipodal pairs, so the guard could be \
             simplified — recheck the derivation before doing it",
        );
        assert_eq!(
            caught_by_epsilon, 2922,
            "the `|sin d| < f64::EPSILON` leakage moved; it was 680 of 3602",
        );
        assert_eq!(
            caught_by_conditioning, total,
            "the conditioning threshold no longer catches every antipodal pair",
        );

        // And what the threshold costs: the shortest interpolable line.
        let reach_m = 2.0 * MIN_CONDITIONING.sqrt().asin() * EARTH_RADIUS_KM * 1000.0;
        assert!(
            (reach_m - 1.519).abs() < 0.001,
            "the guard's reach moved: {reach_m:.4} m, documented as 1.519 m",
        );
        // precondition: that reach is negligible against the data it describes.
        // 250 / 1.519 = 164.6, so the margin is 165× — two orders, not four.
        assert!(
            reach_m < 250.0 / 100.0,
            "precondition: the guard now withdraws over a distance comparable \
             to a 250 m gate ({reach_m:.3} m)",
        );
        assert!(
            (250.0 / reach_m - 164.6).abs() < 0.1,
            "the reach-to-gate margin moved: {:.1}×, documented as 165×",
            250.0 / reach_m,
        );

        // A 230 km section — the real domain — is ten orders clear of it.
        let hav_230 = (230.0 / EARTH_RADIUS_KM / 2.0).sin().powi(2);
        assert!(
            hav_230 / MIN_CONDITIONING > 1e9,
            "a 230 km line is only {:.2e}× the guard's threshold",
            hav_230 / MIN_CONDITIONING,
        );
    }
}
