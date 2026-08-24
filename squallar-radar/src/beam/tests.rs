use super::*;
use squallar_geo::{
    EARTH_RADIUS_KM, great_circle_destination, great_circle_point, site_bearing_range_km,
};

/// The exact spherical height on the same effective sphere — the form
/// `nexrad_model`'s `polar_to_geo` uses, transcribed rather than called.
fn spherical_height_km(slant_range_km: f64, elev_deg: f64) -> f64 {
    let r = slant_range_km;
    let re = RE_EFF_KM;
    (r * r + re * re + 2.0 * r * re * elev_deg.to_radians().sin()).sqrt() - re
}

const ELEVS: [f64; 16] = [
    0.2, 0.5, 0.9, 1.3, 1.8, 2.4, 3.1, 4.0, 5.1, 6.4, 8.0, 10.0, 12.0, 14.0, 16.7, 19.5,
];

/// Float multiplication does not associate, so `volumetric`'s
/// `6371.0 * 4.0 / 3.0` and `nrot`'s `4.0 / 3.0 * 6371.0` were not guaranteed
/// to be the same `f64` — they happen to be. A rounded literal fails here.
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
    assert_ne!(
        RE_EFF_KM.to_bits(),
        8494.667f64.to_bits(),
        "precondition: 8494.667 rounds to the same bits as the exact \
             expression, so this module's headline warning is vacuous",
    );
}

/// The inverse is exact, not fitted, over 1..460 km × the VCP 212 ladder.
/// 1e-9 km is 1 µm: the assertion is that the algebra is right, not close.
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

    // The domain edge: nothing ascends below −Rₑ·sin²e/2, and the inverse says
    // so with `NaN`. −0.32 km at 0.5°, zero at 0°.
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
    assert!(
        slant_range_for_height_km(floor(0.5) + 0.001, 0.5).is_finite(),
        "a height just inside the 0.5° floor was refused",
    );
    assert_eq!(slant_range_for_height_km(0.0, 0.5), 0.0);
}

/// The quadratic against the exact spherical form: negligible where products
/// are drawn, and *not* negligible at 230 km / 19.5°.
#[test]
fn the_quadratic_beam_height_tracks_the_spherical_form_within_the_measured_residual() {
    let resid_m = |r: f64, e: f64| (height_km(r, e) - spherical_height_km(r, e)) * 1000.0;

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

    assert!(
        far_low < 250.0 && near_high < 250.0,
        "a plotted-domain corner now exceeds one 250 m gate: \
             {far_low:.1} m at 230 km/0.5°, {near_high:.1} m at 70 km/19.5°",
    );

    // And the corner that does not: 230 km / 19.5° is ~372 m, larger than a
    // gate, at a beam height of ~79.9 km.
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

    // Nothing below 20 km of altitude exceeds 23.6 m; measured worst 23.489 m.
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
    let at_20km = resid_m(slant_range_for_height_km(20.0, 19.5), 19.5);
    assert!(
        (at_20km - 23.489).abs() < 0.001,
        "the 20 km ceiling residual moved: {at_20km:.4} m, documented as \
             23.489 m",
    );
}

/// The residual is a function of the quadratic height alone:
/// `h_quad − h_sphere = Rₑ·((1+q) − √(1+2q))` with `q = h_quad/Rₑ`, which
/// holds exactly because `r² + Rₑ² + 2rRₑ·sin e ≡ Rₑ² + 2Rₑ·h_quad`.
///
/// The tolerance bounds the measurement, not the identity: the absolute error
/// floor of `spherical_height_km` is `ε·Rₑ` ≈ 1.9e-12 km, measured worst over
/// this grid is 2.575e-12 km, and the tolerance is `4·ε·Rₑ` = 7.5e-12 km.
#[test]
fn the_beam_height_residual_depends_only_on_the_height() {
    // `(1+q)² − q²` is identically `1 + 2q`; the shorter form drops a squaring.
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
    assert_eq!(pairs, ELEVS.len() * 7, "precondition: the grid shrank");
    assert!(
        worst > cancellation_floor,
        "precondition: the worst identity error {worst:e} km is now below \
             the {cancellation_floor:e} km cancellation floor, so the \
             tolerance is slack rather than derived",
    );

    // The spherical form's radicand *is* `Rₑ² + 2·Rₑ·h_quad`. Exact in algebra;
    // in f64 the right-hand side re-rounds one multiply, so 20 of these 7360
    // pairs differ by a single ulp. Bounded at 2 ulps of relative error.
    let mut radicand_pairs = 0usize;
    for &e in &ELEVS {
        for ri in 1..=460 {
            let r = f64::from(ri);
            let direct = r * r + RE_EFF_KM * RE_EFF_KM + 2.0 * r * RE_EFF_KM * e.to_radians().sin();
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

/// Recomputes the expression `volumetric::beam_height_km` shipped —
/// transcribed here, not called — and demands bit-identity.
#[test]
fn the_lifted_beam_height_is_bit_identical_to_the_one_volumetric_shipped() {
    // Verbatim from the pre-lift `volumetric::beam_height_km`.
    fn shipped(range_km: f64, elev_deg: f64) -> f64 {
        const RE_EFF_KM: f64 = 6371.0 * 4.0 / 3.0;
        let el = elev_deg.to_radians();
        range_km * el.sin() + range_km * range_km / (2.0 * RE_EFF_KM)
    }

    let half = WSR88D_HALF_POWER_BEAMWIDTH_DEG / 2.0;
    let mut checked = 0usize;
    for &e in &ELEVS {
        for e in [e - half, e, e + half] {
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

/// The height over a ground range *is* the height over the slant range that
/// reaches the same point, to the bit.
#[test]
fn the_ground_range_height_is_the_slant_range_height_over_the_same_point() {
    let mut checked = 0usize;
    for &e in &ELEVS {
        for r in 1..=460 {
            let r = f64::from(r);
            let g = ground_range_km(r, e);
            let folded = height_at_ground_km(g, e);
            let composed = height_km(slant_range_for_ground_km(g, e), e);
            assert_eq!(
                folded.to_bits(),
                composed.to_bits(),
                "the ground-range and slant-range heights parted at {r} km / \
                 {e}°: {folded} vs {composed}",
            );
            checked += 1;
        }
    }
    assert_eq!(
        checked,
        ELEVS.len() * 460,
        "precondition: the grid did not cover every tilt × range",
    );

    let at_low = 230.0 - ground_range_km(230.0, 2.4);
    let at_high = 70.0 - ground_range_km(70.0, 19.5);
    assert!(
        (at_low - 0.5178).abs() < 1e-3,
        "the 2.4° slant/ground gap moved: {at_low:.4} km, expected 0.5178",
    );
    assert!(
        (at_high - 4.1974).abs() < 1e-3,
        "the 19.5° slant/ground gap moved: {at_high:.4} km, expected 4.1974",
    );
}

/// `ground_range_km` and `slant_range_for_ground_km` are the law of sines read
/// in opposite directions on one triangle, so they cancel algebraically —
/// which is why `ground_range_km` computes the *spherical* height internally
/// rather than reusing the public quadratic `height_km`.
#[test]
fn a_ground_range_round_trip_is_exact_on_both_networks() {
    fn beam_edges(e: f64, full_width_deg: f64) -> [f64; 3] {
        let half = full_width_deg / 2.0;
        [e - half, e, e + half]
    }

    let mut worst = (0.0f64, 0.0f64, 0.0f64);
    let mut points = 0usize;
    let mut check = |r: f64, e: f64| {
        let back = slant_range_for_ground_km(ground_range_km(r, e), e);
        let err = (back - r).abs();
        if err > worst.0 {
            worst = (err, r, e);
        }
        points += 1;
    };

    // A WSR-88D: the VCP 212 ladder out to 460.125 km, stepped at 0.1 km.
    for i in 1..=4601 {
        let r = f64::from(i) * 0.1;
        for &e in &ELEVS {
            for e in beam_edges(e, WSR88D_HALF_POWER_BEAMWIDTH_DEG) {
                check(r, e);
            }
        }
    }

    // A TDWR: its 88.8 km Doppler reach at 0.3 km, climbing to VCP 80's 60°.
    const TDWR_ELEVS: [f64; 17] = [
        0.6, 1.0, 2.0, 3.0, 4.0, 5.0, 6.5, 8.0, 10.0, 12.0, 15.0, 20.0, 25.0, 30.0, 40.0, 50.0,
        60.0,
    ];
    for i in 1..=296 {
        let r = f64::from(i) * 0.3;
        for &e in &TDWR_ELEVS {
            for e in beam_edges(e, TDWR_HALF_POWER_BEAMWIDTH_DEG) {
                check(r, e);
            }
        }
    }

    assert_eq!(
        points, 235_944,
        "precondition: the round-trip grid changed size",
    );
    // Measured worst is 1.7053e-13 km at 316.3 km / 7.525° — 5.4e-16 of that
    // range, two or three ulps, the floor of an `asin`/`sin` round trip.
    assert!(
        worst.0 < 1e-12,
        "the ground-range round trip is not exact: {:e} km at {} km / {}°",
        worst.0,
        worst.1,
        worst.2,
    );
}

/// The arc agrees with `nexrad_model`'s `polar_to_geo`, which spells the
/// effective radius as `6_371_000.0 * 4.0 / 3.0` metres. It never returns the
/// arc, so it is recovered by measuring the great circle to the point it
/// lands on.
///
/// The antenna is at zero and the site's coordinates are read back off the
/// oracle: `polar_to_geo` adds the antenna height before dividing by `Rₑ + h`
/// (21.6 m at 460 km / 0.5° for a 400 m antenna), and `Site` carries lat/lon
/// as `f32`, worth up to a fifth of a metre.
#[test]
fn the_ground_arc_matches_the_nexrad_model_oracle() {
    use nexrad_model::geo::{PolarPoint, RadarCoordinateSystem};
    use nexrad_model::meta::Site;

    let system = RadarCoordinateSystem::new(&Site::new(*b"KTLX", 35.3333, -97.2778, 0, 0));
    let (site_lat, site_lon) = (system.latitude(), system.longitude());

    let mut worst = (0.0f64, 0.0f64, 0.0f64);
    let mut points = 0usize;
    for &e in &ELEVS {
        for i in 1..=460 {
            let r = f64::from(i);
            // `PolarPoint` takes the elevation as `f32`, so narrow it once and
            // widen it back, and give both sides the same angle.
            let e32 = e as f32;
            let e = f64::from(e32);
            for &az in &[0.0f32, 37.5, 123.25, 271.0] {
                let geo = system.polar_to_geo(PolarPoint {
                    azimuth_degrees: az,
                    range_km: r,
                    elevation_degrees: e32,
                });
                let (_, oracle_arc_km) =
                    site_bearing_range_km(site_lat, site_lon, geo.latitude, geo.longitude);
                let err = (ground_range_km(r, e) - oracle_arc_km).abs();
                if err > worst.0 {
                    worst = (err, r, e);
                }
                points += 1;
            }
        }
    }

    assert_eq!(
        points,
        ELEVS.len() * 460 * 4,
        "precondition: the oracle grid changed size",
    );
    // Measured worst is 2.21e-12 km at 52 km / 19.5°, 4e-14 of that range: two
    // trigonometric round trips' worth of rounding. The bound is one micrometre.
    assert!(
        worst.0 < 1e-9,
        "the arc parted from `polar_to_geo` by {:e} km at {} km / {}°",
        worst.0,
        worst.1,
        worst.2,
    );
}

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

    // A degree due east: the initial bearing is *not* 90°, because the great
    // circle between two points at one latitude bows poleward and leaves north
    // of east by ≈ (Δλ/2)·sin φ = 0.2892°. 90° would mean a rhumb-line bearing.
    let (bearing, range) = site_bearing_range_km(site_lat, site_lon, site_lat, site_lon + 1.0);
    let poleward_bow = 0.5 * site_lat.to_radians().sin();
    assert!(
        (bearing - (90.0 - poleward_bow)).abs() < 1e-4,
        "due east read as {bearing}°, expected ~{}° (90° less the \
             {poleward_bow:.4}° poleward bow)",
        90.0 - poleward_bow,
    );

    // And the distance is *shorter* than the parallel it looks like it follows
    // — 0.385 m over one degree here. A flat `Δλ·cos φ` cannot be shorter.
    let parallel = EARTH_RADIUS_KM * std::f64::consts::PI / 180.0 * site_lat.to_radians().cos();
    let chord_saving_m = (parallel - range) * 1000.0;
    assert!(
        (chord_saving_m - 0.385).abs() < 0.01,
        "a degree due east measured {range} km against the {parallel} km \
             parallel arc — a {chord_saving_m:.3} m saving, expected 0.385 m",
    );

    // Degenerate: a site to itself is zero range; the bearing is unconstrained.
    let (_, range) = site_bearing_range_km(site_lat, site_lon, site_lat, site_lon);
    assert_eq!(range, 0.0, "a site is not at zero range from itself");

    assert_eq!(EARTH_RADIUS_KM, 6371.0);
    assert_eq!(
        squallar_geo::KM_PER_DEGREE_LAT.to_bits(),
        (EARTH_RADIUS_KM * std::f64::consts::PI / 180.0).to_bits(),
        "`ImageBounds`' degree has come off this sphere again, so a gate and \
             the geography drawn under it are no longer on one planet",
    );
}

/// Float multiplication does not associate and the two spellings group the
/// radius differently — `Rₑ · (2 · atan2(..))` against `(Rₑ · 2) · atan2(..)`
/// — which agree only because the factor is 2.
/// `(Rₑ · 2) · atan2(..)` — which agree only because the factor is 2.
/// They are not identical everywhere: the copy had no `clamp`, so within
/// ~5e-13 degrees of the site's antipode it produced a `NaN` range where this
/// produces the half-circumference — nine orders below the ~3e-7 degrees one
/// screen pixel spans at the deepest zoom.
#[test]
fn the_hover_readouts_polar_coordinates_are_bit_identical_to_the_deleted_copy() {
    /// Transcribed character-for-character from the deleted block, including
    /// the association of the radius and the absence of a clamp. Do not
    /// "tidy" it: its value is that it is literally what shipped.
    fn deleted_copy(site_lat: f64, site_lon: f64, hover_lat: f64, hover_lon: f64) -> (f64, f64) {
        let lat1 = site_lat.to_radians();
        let lon1 = site_lon.to_radians();
        let lat2 = hover_lat.to_radians();
        let lon2 = hover_lon.to_radians();
        let dlat = lat2 - lat1;
        let dlon = lon2 - lon1;
        let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
        let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
        let distance_km = EARTH_RADIUS_KM * c;

        let y = dlon.sin() * lat2.cos();
        let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * dlon.cos();
        let azimuth = (y.atan2(x).to_degrees() + 360.0) % 360.0;
        (azimuth, distance_km)
    }

    // Nine sites spanning the network: the azimuth's `x` term carries `sin φ`,
    // and PGUA is the only one east of the antimeridian.
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
        for i in -30..=30 {
            for j in -30..=30 {
                let hover_lat = site_lat + f64::from(i) * 0.2137;
                let hover_lon = site_lon + f64::from(j) * 0.2137;
                let (az_old, range_old) = deleted_copy(site_lat, site_lon, hover_lat, hover_lon);
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
    assert_eq!(checked, 33_813, "the comparison grid changed size");

    // Two ulps off KTLX's antipodal latitude is enough to round the haversine
    // over 1.0.
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

    // The load-bearing property: from a site at `a`, the point at `t` is at `t`
    // of the total ground range. A lat/lon lerp fails this.
    let (bearing_b, total) = site_bearing_range_km(a.0, a.1, b.0, b.1);
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
        if step > 0 {
            assert!(
                (bearing - bearing_b).abs() < 1e-6,
                "t={t} bears {bearing}° from the site, not {bearing_b}°",
            );
        }
    }

    let mid = great_circle_point(a, b, 0.5);
    let lerp = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    let (_, sag) = site_bearing_range_km(mid.0, mid.1, lerp.0, lerp.1);
    assert!(
        sag > 0.005,
        "precondition: over a {total:.0} km line the lat/lon lerp is only \
             {:.1} m off the great circle, so this test proves nothing",
        sag * 1000.0,
    );

    let beyond = great_circle_point(a, b, 1.5);
    let (_, range) = site_bearing_range_km(a.0, a.1, beyond.0, beyond.1);
    assert!(
        (range - 1.5 * total).abs() < 1e-6,
        "t=1.5 sits at {range:.6} km, not {:.6}",
        1.5 * total,
    );
}

/// Antipodal is the case worth a test of its own, because two obvious guards
/// do not work and neither fails loudly.
///
/// `sin d == 0.0` catches **none** of 3602 antipodal pairs: `d` is the f64
/// nearest π, whose sine is 1.2246e-16. `|sin d| < f64::EPSILON` catches 2922
/// and misses 680, because a truly antipodal pair does not reliably land `hav`
/// on exactly 1.0 — 2922 (81.1 %) do, 648 (18.0 %) fall one ulp short and 32
/// (0.89 %) two, and `√(1 − hav)` turns even one ulp into `sin d ≈ 2e-8`. Only
/// the well-conditioned `hav` test catches all 3602.
#[test]
fn a_degenerate_pair_of_endpoints_returns_the_first_one() {
    let a = (35.3333, -97.2778);

    assert_eq!(
        great_circle_point(a, a, 0.5),
        a,
        "coincident endpoints did not degenerate to `a`",
    );

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

    // The residual error is bounded in closed form: with `u = 1 − hav`,
    // `d = π − 2√u`, so the range falls short of the half circumference by
    // `Rₑ·2√u` — an error that grows with `u`. `u` is at most `f64::EPSILON`,
    // so the bound is `2·Rₑ·√ε` = 0.18987 m, attained at the 32 pairs of this
    // sweep that land two ulps low. Without the clamp, 3.7 % of these are NaN.
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

    // A non-finite input propagates: `hav` is NaN, which fails both of the
    // guard's comparisons.
    let nan_in = great_circle_point((f64::NAN, 0.0), (1.0, 1.0), 0.5);
    assert!(
        nan_in.0.is_nan() && nan_in.1.is_nan(),
        "a NaN endpoint was silently absorbed, returning {nan_in:?}",
    );
}

/// `64·f64::EPSILON` comes from the conditioning: with `u = 1 − hav`, the
/// divisor `sin d ≈ 2√u` while `hav`'s last ulp reaches `d` amplified to
/// `≈ ε/√u`, so the divisor's relative error is `≈ ε/(2u)` and passes 1 %
/// below ~50ε. It costs 1.519 m of reach — the shortest line the function
/// will still interpolate — which is 165× finer than one 250 m gate.
#[test]
fn the_degeneracy_guard_is_a_derived_threshold() {
    const MIN_CONDITIONING: f64 = 64.0 * f64::EPSILON;

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

    let reach_m = 2.0 * MIN_CONDITIONING.sqrt().asin() * EARTH_RADIUS_KM * 1000.0;
    assert!(
        (reach_m - 1.519).abs() < 0.001,
        "the guard's reach moved: {reach_m:.4} m, documented as 1.519 m",
    );
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

    let hav_230 = (230.0 / EARTH_RADIUS_KM / 2.0).sin().powi(2);
    assert!(
        hav_230 / MIN_CONDITIONING > 1e9,
        "a 230 km line is only {:.2e}× the guard's threshold",
        hav_230 / MIN_CONDITIONING,
    );
}

/// [`great_circle_destination`] is the inverse of [`site_bearing_range_km`].
/// Four sites — KCRP 27.78 °N, KTLX 35.33 °N, KMSX 47.04 °N, KATX 48.19 °N —
/// over every tenth of a degree of bearing and six ranges out to 460 km.
#[test]
fn a_bearing_and_range_round_trip_through_the_destination() {
    let mut worst_range = 0.0f64;
    let mut worst_bearing = 0.0f64;
    for (site_lat, site_lon) in [
        (27.784, -97.511),
        (35.3333, -97.2778),
        (47.0411, -113.986),
        (48.1946, -122.4958),
    ] {
        for i in 0..3600 {
            let bearing = f64::from(i) / 10.0;
            for range in [1.0, 88.8, 150.0, 230.0, 417.0, 460.11] {
                let (lat, lon) = great_circle_destination(site_lat, site_lon, bearing, range);
                let (back_bearing, back_range) =
                    site_bearing_range_km(site_lat, site_lon, lat, lon);
                worst_range = worst_range.max((back_range - range).abs());
                // Wrapped, because 359.9999° and 0.0001° are a ten-thousandth
                // of a degree apart and not 360.
                let d = (back_bearing - bearing).abs();
                worst_bearing = worst_bearing.max(d.min(360.0 - d));
            }
        }
    }
    // Documented on `great_circle_destination` as 3.9e-10 km — 0.39 µm, which
    // is `EARTH_RADIUS_KM`'s own last bits over an `atan2`/`asin` pair.
    assert!(
        worst_range < 1e-9,
        "the range round trip is off by {worst_range:.3e} km",
    );
    assert!(
        worst_bearing < 1e-9,
        "the bearing round trip is off by {worst_bearing:.3e}\u{b0}",
    );
}
