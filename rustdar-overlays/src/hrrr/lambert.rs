//! Lambert Conformal Conic projection, in pure Rust.
//!
//! # Why this exists
//!
//! The `grib` crate can compute grid point lat/lons for GRIB2 grid definition
//! template 3.30 (Lambert conformal) — but only behind its `gridpoints-proj`
//! feature, which links PROJ. PROJ drags in `proj-sys`, `libsqlite3-sys` and
//! `link-cplusplus`, and those C/C++ dependencies cannot cross-compile to
//! `wasm32` or iOS. Building `grib` with `default-features = false` drops them,
//! and with them `Template3_30`'s `latlons()` arm: the template falls into
//! grib's `#[cfg(not(feature = "gridpoints-proj"))]` catch-all and returns
//! `GribError::NotSupported`.
//!
//! HRRR is *entirely* template 3.30, so that would kill every HRRR fetch. This
//! module reimplements the projection so it does not.
//!
//! # What it computes
//!
//! Exactly what `grib` used to hand PROJ, namely
//!
//! ```text
//! +a=<a> +b=<b> +proj=lcc +lat_0=<LaD> +lon_0=<LoV> +lat_1=<Latin1> +lat_2=<Latin2>
//! ```
//!
//! grib's PROJ path forward-projects the grid's *first* point (La1/Lo1) to get
//! the corner in projected metres, steps by Dx/Dy in metres, then inverse
//! projects every step back to lat/lon. [`latlons`] reproduces that sequence,
//! which is why both a forward and an inverse are implemented here rather than
//! just the inverse.
//!
//! # The math
//!
//! Snyder, *Map Projections — A Working Manual* (USGS Professional Paper 1395),
//! chapter 15 "Lambert Conformal Conic". The ellipsoidal formulation is used
//! throughout; it reduces exactly to the spherical one when the eccentricity is
//! zero, which is the case for HRRR (see [`LambertConformalConic::new`]).
//!
//! Both the secant case (`lat_1 != lat_2`) and the tangent case
//! (`lat_1 == lat_2`, where Snyder eq. 15-4 for the cone constant is 0/0 and
//! must be replaced by its limit `n = sin(lat_1)`) are implemented. **HRRR is
//! the tangent case** — its Latin1 and Latin2 are both 38.5° — so the tangent
//! limit is not an edge case here, it is the only path HRRR ever takes.

use grib::{GridPointIndex, def::grib2::template::Template3_30};

/// Below this separation (in radians) the two standard parallels are treated as
/// coincident and the tangent limit `n = sin(lat_1)` is used.
///
/// GRIB2 stores Latin1/Latin2 as integer microdegrees, so the smallest non-zero
/// separation representable is 1e-6° = 1.745e-8 rad. This threshold sits an
/// order of magnitude below that, so a genuinely secant grid is never mistaken
/// for a tangent one. It still guards the secant formula against catastrophic
/// cancellation: as `lat_1 -> lat_2` both the numerator and denominator of
/// eq. 15-4 approach zero, and below this separation the tangent limit is more
/// accurate than the ratio of two near-zero differences.
const TANGENT_EPSILON_RAD: f64 = 1e-9;

/// Convergence threshold for the ellipsoidal latitude iteration, in radians.
/// 1e-12 rad is ~6 micrometres on the ground — far below any meaningful
/// tolerance, and reached in a handful of iterations for terrestrial
/// eccentricities.
const LATITUDE_ITERATION_TOLERANCE_RAD: f64 = 1e-12;

/// Iteration cap for the ellipsoidal latitude solve. Terrestrial eccentricities
/// converge in well under ten passes; the cap only exists so a pathological
/// ellipsoid cannot hang a fetch.
const MAX_LATITUDE_ITERATIONS: usize = 32;

/// A Lambert Conformal Conic projection.
///
/// Constructed from the same six parameters PROJ's `+proj=lcc` takes, so it can
/// be checked against PROJ directly. See [`LambertConformalConic::new`].
#[derive(Debug, Clone, Copy)]
pub struct LambertConformalConic {
    /// Semi-major axis, metres.
    a: f64,
    /// First eccentricity. Exactly 0 for a sphere, which short-circuits the
    /// iterative inverse.
    e: f64,
    /// Cone constant (Snyder eq. 15-4, or its tangent limit `sin(lat_1)`).
    n: f64,
    /// Scale factor (Snyder eq. 15-2).
    big_f: f64,
    /// Polar radius to the latitude of origin (Snyder eq. 15-1a).
    rho0: f64,
    /// Central meridian, radians.
    lon0: f64,
}

impl LambertConformalConic {
    /// Build a projection from ellipsoid axes and the four angular parameters.
    ///
    /// * `a`, `b` — semi-major and semi-minor axes in metres. `a == b` gives a
    ///   sphere. **HRRR's GRIB2 section 3 carries earth-shape code 6**, which
    ///   WMO Code Table 3.2 defines as "spherical with radius 6,371,229.0 m" —
    ///   *not* the 6,371,200 m of code 8, and not an ellipsoid. Getting this
    ///   wrong displaces the whole grid by tens of metres without making it
    ///   look wrong. Callers should take the radii from
    ///   `EarthShape::radii()` rather than hardcoding, as [`latlons`] does.
    /// * `lat_0` — latitude of origin (GRIB2 `LaD`), degrees.
    /// * `lon_0` — central meridian (GRIB2 `LoV`), degrees. May be given in
    ///   either 0..360 or -180..180 form.
    /// * `lat_1`, `lat_2` — standard parallels (GRIB2 `Latin1`/`Latin2`),
    ///   degrees. Equal values select the tangent case.
    pub fn new(
        a: f64,
        b: f64,
        lat_0: f64,
        lon_0: f64,
        lat_1: f64,
        lat_2: f64,
    ) -> Result<Self, String> {
        if !(a.is_finite() && b.is_finite()) || a <= 0.0 || b <= 0.0 {
            return Err(format!("Lambert conformal: bad earth axes a={a}, b={b}"));
        }
        if b > a {
            return Err(format!(
                "Lambert conformal: semi-minor axis {b} exceeds semi-major axis {a}"
            ));
        }

        let phi0 = lat_0.to_radians();
        let phi1 = lat_1.to_radians();
        let phi2 = lat_2.to_radians();

        // A standard parallel at a pole gives a cone of zero radius, and
        // `lat_1 == -lat_2` gives a cone constant of zero (the cone opens into
        // a cylinder — that is Mercator, not LCC). Neither is representable.
        let quarter_turn = std::f64::consts::FRAC_PI_2;
        for (name, phi) in [("Latin1", phi1), ("Latin2", phi2)] {
            if phi.abs() >= quarter_turn {
                return Err(format!(
                    "Lambert conformal: standard parallel {name}={} is at or beyond a pole",
                    phi.to_degrees()
                ));
            }
        }
        if phi0.abs() > quarter_turn {
            return Err(format!("Lambert conformal: LaD={lat_0} is not a latitude"));
        }

        // Eccentricity. `a == b` must yield *exactly* zero so the spherical
        // short-circuit in `inverse` is taken; `1 - (b/a)^2` does that.
        let e = (1.0 - (b / a) * (b / a)).max(0.0).sqrt();

        let m1 = Self::m(phi1, e);
        let t1 = Self::t(phi1, e);

        let n = if (phi1 - phi2).abs() < TANGENT_EPSILON_RAD {
            // Tangent cone. Snyder eq. 15-4 is 0/0 here; its limit as
            // lat_2 -> lat_1 is sin(lat_1). This is the branch HRRR takes.
            phi1.sin()
        } else {
            // Secant cone, Snyder eq. 15-4.
            let m2 = Self::m(phi2, e);
            let t2 = Self::t(phi2, e);
            (m1.ln() - m2.ln()) / (t1.ln() - t2.ln())
        };

        if !n.is_finite() || n == 0.0 {
            return Err(format!(
                "Lambert conformal: degenerate cone constant {n} for \
                 Latin1={lat_1}, Latin2={lat_2} (parallels symmetric about the \
                 equator describe a cylinder, not a cone)"
            ));
        }

        // Snyder eq. 15-2 and 15-1a.
        let big_f = m1 / (n * t1.powf(n));
        let rho0 = a * big_f * Self::t(phi0, e).powf(n);

        if !(big_f.is_finite() && rho0.is_finite()) {
            return Err(format!(
                "Lambert conformal: non-finite projection constants (F={big_f}, rho0={rho0})"
            ));
        }

        Ok(Self {
            a,
            e,
            n,
            big_f,
            rho0,
            lon0: lon_0.to_radians(),
        })
    }

    /// Snyder eq. 14-15: `m = cos(phi) / sqrt(1 - e^2 sin^2 phi)`.
    fn m(phi: f64, e: f64) -> f64 {
        let s = phi.sin();
        phi.cos() / (1.0 - e * e * s * s).sqrt()
    }

    /// Snyder eq. 15-9: `t = tan(pi/4 - phi/2) / ((1 - e sin phi)/(1 + e sin phi))^(e/2)`.
    ///
    /// Reduces to `tan(pi/4 - phi/2)` when `e == 0`.
    fn t(phi: f64, e: f64) -> f64 {
        let s = phi.sin();
        let numerator = (std::f64::consts::FRAC_PI_4 - phi / 2.0).tan();
        if e == 0.0 {
            numerator
        } else {
            numerator / ((1.0 - e * s) / (1.0 + e * s)).powf(e / 2.0)
        }
    }

    /// Project geodetic degrees to projection-plane metres.
    ///
    /// Snyder eq. 15-1, 15-2, 14-4. The origin of the returned coordinates is
    /// `(lon_0, lat_0)`; there is no false easting/northing, matching the PROJ
    /// string grib builds.
    pub fn forward(&self, lat_deg: f64, lon_deg: f64) -> (f64, f64) {
        let phi = lat_deg.to_radians();
        let rho = self.a * self.big_f * Self::t(phi, self.e).powf(self.n);
        let theta = self.n * normalize_radians(lon_deg.to_radians() - self.lon0);
        (rho * theta.sin(), self.rho0 - rho * theta.cos())
    }

    /// Inverse: projection-plane metres back to geodetic degrees.
    ///
    /// Snyder eq. 15-5 through 15-11 (ellipsoidal) / 14-9 (spherical). The
    /// returned longitude is *not* wrapped into -180..180; [`latlons`] does
    /// that, matching grib's own `latlons` / `latlons_unchecked` split.
    pub fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let sign = if self.n < 0.0 { -1.0 } else { 1.0 };
        let dy = self.rho0 - y;

        // Snyder eq. 15-11: rho carries the sign of n.
        let rho = sign * (x * x + dy * dy).sqrt();
        // Snyder eq. 14-11, with both arguments scaled by sign(n) so the
        // southern-cone case lands in the right quadrant.
        let theta = (sign * x).atan2(sign * dy);

        let lon = theta / self.n + self.lon0;

        // At the cone apex rho is zero and the latitude is the pole itself;
        // `powf` and `atan` handle that without a special case (0^k = 0,
        // atan(0) = 0 => phi = pi/2), so only guard the truly undefined ratio.
        let denom = self.a * self.big_f;
        let t = if denom == 0.0 {
            0.0
        } else {
            (rho / denom).powf(1.0 / self.n)
        };

        let lat = if self.e == 0.0 {
            // Spherical: closed form, Snyder eq. 14-9 rearranged.
            std::f64::consts::FRAC_PI_2 - 2.0 * t.atan()
        } else {
            // Ellipsoidal: Snyder eq. 7-9, iterated. Seeded with the spherical
            // solution, which is already within ~0.2° for Earth-like flattening.
            let mut phi = std::f64::consts::FRAC_PI_2 - 2.0 * t.atan();
            for _ in 0..MAX_LATITUDE_ITERATIONS {
                let s = phi.sin();
                let next = std::f64::consts::FRAC_PI_2
                    - 2.0 * (t * ((1.0 - self.e * s) / (1.0 + self.e * s)).powf(self.e / 2.0)).atan();
                let converged = (next - phi).abs() < LATITUDE_ITERATION_TOLERANCE_RAD;
                phi = next;
                if converged {
                    break;
                }
            }
            phi
        };

        (lat.to_degrees(), lon.to_degrees())
    }
}

/// Wrap radians into `-pi..=pi`.
///
/// GRIB2 stores LoV and Lo1 as unsigned microdegrees (HRRR: 262500000 and
/// 237280472, i.e. 262.5° and 237.28°), so their difference is already small —
/// but a grid straddling the antimeridian would not be, and `theta` must be the
/// *short* way round for the forward projection to land in the right quadrant.
fn normalize_radians(mut r: f64) -> f64 {
    use std::f64::consts::{PI, TAU};
    if !r.is_finite() {
        return r;
    }
    while r > PI {
        r -= TAU;
    }
    while r < -PI {
        r += TAU;
    }
    r
}

/// Wrap degrees into `-180..180`, matching grib's `normalize_latlon`.
fn normalize_longitude_degrees(lon: f64) -> f64 {
    (lon + 540.0).rem_euclid(360.0) - 180.0
}

/// Compute lat/lon for every point of a template 3.30 (Lambert conformal) grid.
///
/// This is the drop-in replacement for `grib`'s PROJ-backed
/// `<Template3_30 as LatLons>::latlons`, and deliberately mirrors its sequence:
/// forward-project the first grid point, walk the grid in projected metres
/// using Dx/Dy signed by the scanning mode, then inverse-project each step.
///
/// Points come back in scanning-mode order — the same order as the decoded data
/// values — because the iteration is driven by grib's own
/// [`GridPointIndex::ij`], which is not gated behind `gridpoints-proj`.
pub fn latlons(grid: &Template3_30) -> Result<Vec<(f64, f64)>, String> {
    // Template 3.30 fixes the angular unit at 1e-6 degrees; there is no basic
    // angle / subdivision pair in this template to override it.
    const ANGLE_UNIT: f64 = 1e-6;
    // Dx and Dy are in millimetres for this template.
    const LENGTH_UNIT_M: f64 = 1e-3;

    let lad = f64::from(grid.lad) * ANGLE_UNIT;
    let lov = f64::from(grid.lov) * ANGLE_UNIT;
    let latin1 = f64::from(grid.latin1) * ANGLE_UNIT;
    let latin2 = f64::from(grid.latin2) * ANGLE_UNIT;

    let (a, b) = grid.earth_shape.radii().ok_or_else(|| {
        format!(
            "Unknown value of GRIB2 Code Table 3.2 (shape of the Earth): {}",
            grid.earth_shape.shape
        )
    })?;

    let projection = LambertConformalConic::new(a, b, lad, lov, latin1, latin2)?;

    // Grid steps are always positive in the encoding; the scanning mode says
    // which way they actually run.
    let mut dx = f64::from(grid.dx) * LENGTH_UNIT_M;
    let mut dy = f64::from(grid.dy) * LENGTH_UNIT_M;
    if !grid.scanning_mode.scans_positively_for_i() && dx > 0.0 {
        dx = -dx;
    }
    if !grid.scanning_mode.scans_positively_for_j() && dy > 0.0 {
        dy = -dy;
    }

    let (x0, y0) = projection.forward(
        f64::from(grid.first_point_lat) * ANGLE_UNIT,
        f64::from(grid.first_point_lon) * ANGLE_UNIT,
    );

    let indices = grid
        .ij()
        .map_err(|e| format!("Cannot iterate Lambert grid indices: {e}"))?;

    Ok(indices
        .map(|(i, j)| {
            let (lat, lon) = projection.inverse(x0 + dx * i as f64, y0 + dy * j as f64);
            (lat, normalize_longitude_degrees(lon))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Independently-sourced reference values.
    //
    // Nothing below is computed with the code under test. Provenance is
    // recorded per constant; the three sources are:
    //
    //   (A) The HRRR GRIB2 file itself. Section 3 states La1/Lo1 — the lat/lon
    //       of grid point (0,0) — so the file hands us the expected value for
    //       one corner outright.
    //   (B) PROJ 9.8.1 (`proj -I +proj=lcc ...`, system binary, PROJ release of
    //       2026-04-10). This is a separate C implementation, and specifically
    //       the one `grib`'s `gridpoints-proj` feature delegated to, so it is
    //       the exact behaviour this module must preserve.
    //   (C) The EPSG Guidance Note 7-2 published worked example for
    //       "Lambert Conic Conformal (2SP)" (EPSG method 9802), whose expected
    //       coordinates are printed in the document.
    // ------------------------------------------------------------------

    /// HRRR CONUS grid definition, transcribed field-for-field from GRIB2
    /// section 3 (81 octets, template 3.30) of a **raw operational** HRRR file:
    /// `hrrr.t12z.wrfsfcf00.grib2` (2026-07-24 12Z, CIN/surface), fetched
    /// without any `subregion` argument so NOMADS streams the record through
    /// untouched.
    ///
    /// These are also the values `wgrib2 -grid` reports for any HRRR CONUS
    /// file, and match NOAA's published HRRR domain specification:
    /// 1799x1059, 3 km, LoV 262.5, LaD/Latin1/Latin2 38.5.
    ///
    /// # If you re-derive this from what the app actually downloads, read this
    ///
    /// You will get a *different* `Lo1` and the corner assertions will fail by
    /// a hair. That is expected and is not a projection bug — see
    /// [`the_nomads_filter_reencodes_lo1_by_one_microdegree`], which pins the
    /// other value. The app's request carries `subregion=&toplat=...`, which
    /// makes NOMADS re-encode the record through wgrib2 rather than stream it;
    /// re-encoding re-rounds `Lo1` from 237280472 to 237280471 microdegrees.
    /// The projection anchors on whatever `Lo1` the file states, so both are
    /// correct in production; this fixture uses the operational value because
    /// that is the canonical, published grid definition and does not depend on
    /// NOMADS' tooling.
    fn hrrr_conus_grid() -> Template3_30 {
        use grib::def::grib2::template::param_set;
        Template3_30 {
            earth_shape: param_set::EarthShape {
                // Code Table 3.2 value 6 = "spherical, radius 6,371,229.0 m".
                // The radius is implied by the code; the explicit radius
                // fields are all zero in the file, as transcribed here.
                shape: 6,
                spherical_earth_radius_scale_factor: 0,
                spherical_earth_radius_scaled_value: 0,
                major_axis_scale_factor: 0,
                major_axis_scaled_value: 0,
                minor_axis_scale_factor: 0,
                minor_axis_scaled_value: 0,
            },
            ni: 1799,
            nj: 1059,
            first_point_lat: 21_138_123,
            first_point_lon: 237_280_472,
            resolution_and_component_flags: param_set::ResolutionAndComponentFlags(0b0000_1000),
            lad: 38_500_000,
            lov: 262_500_000,
            dx: 3_000_000,
            dy: 3_000_000,
            projection_centre: param_set::ProjectionCentreFlag(0b0000_0000),
            // 0b0100_0000: +i, +j, i-consecutive, no alternating rows.
            scanning_mode: param_set::ScanningMode(0b0100_0000),
            latin1: 38_500_000,
            latin2: 38_500_000,
            south_pole_lat: 0,
            south_pole_lon: 0,
        }
    }

    /// Tolerance for HRRR grid-point comparisons, in degrees.
    ///
    /// **On the ground this is about 11 cm.** One degree of latitude is
    /// ~111.2 km everywhere, so 1e-6° = 0.111 m. One degree of longitude is
    /// ~111.32*cos(lat) km, which over HRRR's 21°..48° span is 103.9 km down to
    /// 74.5 km, so 1e-6° of longitude is between 0.104 m and 0.075 m. The
    /// bound is therefore <= 0.111 m anywhere on the grid.
    ///
    /// This is far tighter than the ~3 km grid spacing and far tighter than any
    /// rendering could resolve, but it is deliberately not slack: the failure
    /// mode this guards against is a *systematic* displacement (wrong earth
    /// radius, dropped central meridian), which shifts points by tens of metres
    /// at minimum — two to six orders of magnitude above this bound. A loose
    /// tolerance would hide exactly the bug worth catching.
    const HRRR_TOLERANCE_DEG: f64 = 1e-6;

    #[track_caller]
    fn assert_latlon_close(actual: (f64, f64), expected: (f64, f64), tol: f64, what: &str) {
        let (dlat, dlon) = (
            (actual.0 - expected.0).abs(),
            (actual.1 - expected.1).abs(),
        );
        assert!(
            dlat <= tol && dlon <= tol,
            "{what}: got ({:.8}, {:.8}), expected ({:.8}, {:.8}); \
             off by ({dlat:.3e}, {dlon:.3e})° > {tol:.1e}°",
            actual.0,
            actual.1,
            expected.0,
            expected.1,
        );
    }

    fn index_of(grid: &Template3_30, i: usize, j: usize) -> usize {
        // Scanning mode 0b0100_0000 is i-consecutive, so rows of `ni` points.
        j * grid.ni as usize + i
    }

    /// **Source (A) — the file's own section 3.**
    ///
    /// La1/Lo1 *is* the lat/lon of grid point (0,0): 21.138123°N,
    /// 237.280472°E = -122.719528°E. Reproducing it is a check the data hands
    /// us for free, with no external tool in the loop at all.
    ///
    /// Note what this does *not* prove. `latlons` forward-projects La1/Lo1 and
    /// immediately inverse-projects the same point, so corner (0,0) is
    /// algebraically invariant under any change to the earth radius, and
    /// nearly so under changes to LaD. That is precisely why the far-corner
    /// tests below exist and why this one is not sufficient on its own.
    #[test]
    fn first_grid_point_reproduces_the_files_own_la1_lo1() {
        let grid = hrrr_conus_grid();
        let points = latlons(&grid).unwrap();
        assert_eq!(points.len(), 1799 * 1059);
        assert_latlon_close(
            points[0],
            // Straight from section 3: La1 = 21138123, Lo1 = 237280472,
            // both in microdegrees, longitude wrapped to -180..180.
            (21.138123, 237.280472 - 360.0),
            HRRR_TOLERANCE_DEG,
            "grid point (0,0) vs the file's La1/Lo1",
        );
    }

    /// **Source (B) — PROJ 9.8.1.**
    ///
    /// Generated by forward-projecting La1/Lo1 with PROJ, stepping by
    /// 3000 m per index exactly as `latlons` does, and inverse-projecting:
    ///
    /// ```text
    /// $ echo "-122.719528 21.138123" | proj -f "%.6f" \
    ///     +proj=lcc +a=6371229 +b=6371229 \
    ///     +lat_0=38.5 +lon_0=262.5 +lat_1=38.5 +lat_2=38.5
    /// -2697520.142522  -1587306.152557
    ///
    /// $ proj -I -f "%.8f" +proj=lcc +a=6371229 +b=6371229 \
    ///     +lat_0=38.5 +lon_0=262.5 +lat_1=38.5 +lat_2=38.5 <<EOF
    /// -2697520.142522 -1587306.152557     # (i,j) = (   0,    0)
    ///  2696479.857478 -1587306.152557     # (i,j) = (1798,    0)
    /// -2697520.142522  1586693.847443     # (i,j) = (   0, 1058)
    ///  2696479.857478  1586693.847443     # (i,j) = (1798, 1058)
    ///  -520.142522     -306.152557        # (i,j) = ( 899,  529)
    /// EOF
    /// -122.71952800  21.13812300
    ///  -72.28971849  21.14054663
    /// -134.09547973  47.83862350
    ///  -60.91719277  47.84219502
    ///  -97.50597669  38.49724665
    /// ```
    ///
    /// PROJ's inverse of the first corner returns La1/Lo1 to all eight printed
    /// decimals, which cross-validates source (B) against source (A).
    const HRRR_CORNERS_FROM_PROJ: &[(usize, usize, f64, f64, &str)] = &[
        (0, 0, 21.138123_00, -122.719528_00, "SW corner (0,0)"),
        (1798, 0, 21.140546_63, -72.289718_49, "SE corner (ni-1,0)"),
        (0, 1058, 47.838623_50, -134.095479_73, "NW corner (0,nj-1)"),
        (1798, 1058, 47.842195_02, -60.917192_77, "NE corner (ni-1,nj-1)"),
        (899, 529, 38.497246_65, -97.505976_69, "interior midpoint"),
    ];

    /// All four corners plus an interior point against PROJ.
    ///
    /// The interior point is not decoration: it sits ~500 m from the projection
    /// origin (LoV 262.5 = -97.5, LaD 38.5), so it pins the origin placement
    /// that the corners — all thousands of kilometres away — constrain only
    /// weakly.
    #[test]
    fn hrrr_corners_and_interior_match_proj() {
        let grid = hrrr_conus_grid();
        let points = latlons(&grid).unwrap();
        for &(i, j, lat, lon, what) in HRRR_CORNERS_FROM_PROJ {
            assert_latlon_close(
                points[index_of(&grid, i, j)],
                (lat, lon),
                HRRR_TOLERANCE_DEG,
                what,
            );
        }
    }

    /// The grid the app actually downloads has a different `Lo1`, and it also
    /// projects correctly.
    ///
    /// `nomads_url` always appends `subregion=&toplat=...&leftlon=...`. Those
    /// arguments do not subset a Lambert grid — the response is always the full
    /// 1799x1059 CONUS field — but they are *not* inert: they make NOMADS
    /// re-encode the record through wgrib2 instead of streaming it, and the
    /// re-encode re-rounds `Lo1` from 237280472 to **237280471** microdegrees.
    /// (It also re-packs the data from DRT 5.3 to DRT 5.0; both are pure Rust
    /// in grib, which is why dropping JPEG2000 and CCSDS is safe either way.)
    ///
    /// One microdegree of `Lo1` is ~10 cm at the anchor, but it is a *rotation*
    /// of the whole grid about the cone apex, so it grows with distance: at the
    /// NE corner the two encodings differ by 1.12e-6°, which is just over
    /// [`HRRR_TOLERANCE_DEG`]. Hence this is its own fixture rather than a
    /// looser tolerance on the shared one — a tolerance wide enough to accept
    /// both would be wide enough to hide a real error.
    ///
    /// **Source (B) — PROJ 9.8.1**, same procedure as
    /// [`HRRR_CORNERS_FROM_PROJ`] but anchored at Lo1 = 237.280471:
    ///
    /// ```text
    /// $ echo "-122.719529 21.138123" | proj -f "%.6f" +proj=lcc \
    ///     +a=6371229 +b=6371229 +lat_0=38.5 +lon_0=262.5 +lat_1=38.5 +lat_2=38.5
    /// -2697520.246793  -1587306.123248
    ///
    /// $ proj -I -f "%.8f" ...same...   # corner offsets at 3000 m per index
    /// -122.71952900  21.13812300
    ///  -72.28971934  21.14054711
    /// -134.09548115  47.83862338
    ///  -60.91719389  47.84219562
    ///  -97.50597789  38.49724691
    /// ```
    #[test]
    fn the_nomads_filter_reencodes_lo1_by_one_microdegree() {
        let mut grid = hrrr_conus_grid();
        grid.first_point_lon = 237_280_471;
        let points = latlons(&grid).unwrap();

        let from_proj: &[(usize, usize, f64, f64, &str)] = &[
            (0, 0, 21.138123_00, -122.719529_00, "SW"),
            (1798, 0, 21.140547_11, -72.289719_34, "SE"),
            (0, 1058, 47.838623_38, -134.095481_15, "NW"),
            (1798, 1058, 47.842195_62, -60.917193_89, "NE"),
            (899, 529, 38.497246_91, -97.505977_89, "interior"),
        ];
        for &(i, j, lat, lon, what) in from_proj {
            assert_latlon_close(
                points[index_of(&grid, i, j)],
                (lat, lon),
                HRRR_TOLERANCE_DEG,
                &format!("filter-CGI encoding, {what}"),
            );
        }

        // And the trap itself: the two encodings really do disagree by more
        // than the shared tolerance at the far corner, so swapping one fixture
        // for the other silently is not an option.
        let operational = latlons(&hrrr_conus_grid()).unwrap();
        let ne = index_of(&grid, 1798, 1058);
        let delta = (points[ne].1 - operational[ne].1).abs();
        assert!(
            delta > HRRR_TOLERANCE_DEG,
            "the two encodings differ by {delta:.3e}° at the NE corner, which is \
             within tolerance — this test no longer documents anything",
        );
        assert!(
            delta < 2.0 * HRRR_TOLERANCE_DEG,
            "the two encodings differ by {delta:.3e}° at the NE corner, far more \
             than one microdegree of Lo1 should cause",
        );
    }

    /// The grid must actually cover CONUS, in the orientation HRRR states.
    ///
    /// A projection can agree with PROJ at sampled points and still be indexed
    /// wrongly — transposed axes, or a sign flip on Dy — so this asserts the
    /// gross shape independently: scanning mode 0b0100_0000 scans +i (west to
    /// east) and +j (south to north), so latitude must increase with j and the
    /// full-grid extent must bracket the continental US.
    #[test]
    fn grid_spans_conus_in_the_stated_scan_order() {
        let grid = hrrr_conus_grid();
        let points = latlons(&grid).unwrap();

        let sw = points[index_of(&grid, 0, 0)];
        let nw = points[index_of(&grid, 0, 1058)];
        let se = points[index_of(&grid, 1798, 0)];
        assert!(nw.0 > sw.0, "+j must run south to north: {sw:?} -> {nw:?}");
        assert!(se.1 > sw.1, "+i must run west to east: {sw:?} -> {se:?}");

        let (mut min_lat, mut max_lat) = (f64::MAX, f64::MIN);
        let (mut min_lon, mut max_lon) = (f64::MAX, f64::MIN);
        for &(lat, lon) in &points {
            min_lat = min_lat.min(lat);
            max_lat = max_lat.max(lat);
            min_lon = min_lon.min(lon);
            max_lon = max_lon.max(lon);
        }
        // NOAA's published HRRR CONUS domain: roughly 21.1N-52.6N,
        // 134.1W-60.9W. Bracketed loosely — this test is about gross
        // orientation, the tight numbers are the PROJ test's job.
        assert!(
            (21.0..22.0).contains(&min_lat) && (52.0..53.0).contains(&max_lat),
            "latitude span {min_lat}..{max_lat} is not the HRRR CONUS domain",
        );
        assert!(
            (-135.0..-133.0).contains(&min_lon) && (-61.5..-60.0).contains(&max_lon),
            "longitude span {min_lon}..{max_lon} is not the HRRR CONUS domain",
        );
    }

    /// Round trip: inverse then forward must land back on the original grid
    /// index, to well under a grid cell.
    ///
    /// This is independent of every reference value above — it only asserts
    /// that `forward` and `inverse` are mutual inverses and that the grid walk
    /// is consistent — so it catches an internally-consistent-but-wrong
    /// parameterisation only in combination with the PROJ test, and catches
    /// forward/inverse asymmetry that the PROJ test cannot see at all.
    #[test]
    fn forward_of_inverse_returns_the_original_grid_index() {
        let grid = hrrr_conus_grid();
        let projection = LambertConformalConic::new(6_371_229.0, 6_371_229.0, 38.5, 262.5, 38.5, 38.5)
            .unwrap();
        let (x0, y0) = projection.forward(21.138123, 237.280472);
        let points = latlons(&grid).unwrap();

        for &(i, j, _, _, what) in HRRR_CORNERS_FROM_PROJ {
            let (lat, lon) = points[index_of(&grid, i, j)];
            let (x, y) = projection.forward(lat, lon);
            let (fi, fj) = ((x - x0) / 3000.0, (y - y0) / 3000.0);
            // 1e-6 of a 3 km cell is 3 mm.
            assert!(
                (fi - i as f64).abs() < 1e-6 && (fj - j as f64).abs() < 1e-6,
                "{what}: round trip gave index ({fi}, {fj}), expected ({i}, {j})",
            );
        }
    }

    /// **Hand-worked invariant, no external source needed.**
    ///
    /// The projection origin is by construction the point `(lon_0, lat_0)`:
    /// at `phi = lat_0`, `rho = rho0` (eq. 15-1a is eq. 15-1 evaluated there),
    /// and at `lambda = lon_0`, `theta = 0`. So eq. 14-1/14-2 give
    /// `x = rho0*sin(0) = 0` and `y = rho0 - rho0*cos(0) = 0`.
    ///
    /// Therefore `inverse(0, 0) == (lat_0, lon_0)` exactly, for *any* valid
    /// parameter set. A cone constant, scale factor or origin radius that is
    /// individually wrong but self-consistent can survive a single-point check
    /// elsewhere; it cannot survive this one across all four cases below.
    #[test]
    fn projection_origin_inverts_to_lat0_lon0() {
        let cases: &[(f64, f64, f64, f64, f64, f64, &str)] = &[
            // (a, b, lat_0, lon_0, lat_1, lat_2, label)
            (6_371_229.0, 6_371_229.0, 38.5, 262.5, 38.5, 38.5, "HRRR (tangent, sphere)"),
            (6_371_229.0, 6_371_229.0, 38.5, -97.5, 30.0, 60.0, "secant, sphere"),
            (6_378_206.4, 6_356_583.8, 27.833333, -99.0, 28.383333, 30.283333, "secant, ellipsoid"),
            (6_371_229.0, 6_371_229.0, -38.5, 145.0, -30.0, -60.0, "southern cone"),
        ];
        for &(a, b, lat0, lon0, lat1, lat2, label) in cases {
            let p = LambertConformalConic::new(a, b, lat0, lon0, lat1, lat2).unwrap();
            let (lat, lon) = p.inverse(0.0, 0.0);
            assert!(
                (lat - lat0).abs() < 1e-9 && (lon - lon0).abs() < 1e-9,
                "{label}: inverse(0,0) = ({lat}, {lon}), expected ({lat0}, {lon0})",
            );
        }
    }

    /// **Hand-worked invariant.**
    ///
    /// For a tangent cone the cone constant is `sin(lat_1)`. HRRR's standard
    /// parallel is 38.5°, so `n = sin(38.5°) = 0.6225146366376195`, computed
    /// here from `f64::sin` rather than from any projection code.
    ///
    /// This matters because the tangent branch is the *only* branch HRRR uses,
    /// and because it is the branch whose formula is not the textbook one —
    /// Snyder eq. 15-4 is 0/0 at `lat_1 == lat_2` and would give NaN.
    #[test]
    fn tangent_cone_constant_is_sin_of_the_standard_parallel() {
        let tangent =
            LambertConformalConic::new(6_371_229.0, 6_371_229.0, 38.5, 262.5, 38.5, 38.5).unwrap();
        let expected = 38.5_f64.to_radians().sin();
        assert!(
            (tangent.n - expected).abs() < 1e-15,
            "tangent cone constant {} != sin(38.5°) = {expected}",
            tangent.n,
        );
        assert!(tangent.n.is_finite(), "tangent branch produced {}", tangent.n);
    }

    /// The tangent cone constant must come from **Latin1, not LaD**.
    ///
    /// Every other test in this file uses HRRR or an HRRR-shaped fixture, where
    /// `LaD == Latin1 == Latin2 == 38.5°`. Under that coincidence
    /// `n = sin(LaD)` and `n = sin(Latin1)` are indistinguishable, so mutating
    /// one into the other survives the whole suite — the code was right but
    /// unpinned. GRIB2 template 3.30 carries LaD and Latin1/Latin2 as separate
    /// fields and does not require them to agree, so this fixture separates
    /// them: LaD = 45°, Latin1 = Latin2 = 30°.
    ///
    /// `n` must therefore be `sin(30°) = 0.5`, not `sin(45°) = 0.7071`.
    ///
    /// **Source (B) — PROJ 9.8.1:**
    ///
    /// ```text
    /// $ proj -f "%.6f" +proj=lcc +a=6371229 +b=6371229 \
    ///       +lat_0=45 +lon_0=-97.5 +lat_1=30 +lat_2=30 <<EOF
    /// -97.5 30
    /// -110 25
    /// -85 40
    /// -97.5 45
    /// EOF
    ///        0.000000  -1688204.721066
    /// -1261983.214498  -2175998.860672
    ///  1079682.808600   -511425.736939
    ///        0.000000         0.000000
    /// ```
    ///
    /// The last row is the origin invariant, which holds under the mutation too
    /// (rho0 is built from the same `n`, so the apex moves with it) — which is
    /// exactly why the other three rows are needed.
    #[test]
    fn tangent_cone_constant_follows_latin1_not_lad() {
        let p = LambertConformalConic::new(6_371_229.0, 6_371_229.0, 45.0, -97.5, 30.0, 30.0)
            .unwrap();

        let expected_n = 30.0_f64.to_radians().sin();
        assert!(
            (p.n - expected_n).abs() < 1e-15,
            "cone constant {} is not sin(Latin1) = sin(30°) = {expected_n}; \
             sin(LaD) = sin(45°) = {} would be wrong",
            p.n,
            45.0_f64.to_radians().sin(),
        );

        // Tolerance is 1 mm, the printed precision of the reference.
        let from_proj: &[(f64, f64, f64, f64)] = &[
            (30.0, -97.5, 0.0, -1_688_204.721066),
            (25.0, -110.0, -1_261_983.214498, -2_175_998.860672),
            (40.0, -85.0, 1_079_682.808600, -511_425.736939),
            (45.0, -97.5, 0.0, 0.0),
        ];
        for &(lat, lon, want_x, want_y) in from_proj {
            let (x, y) = p.forward(lat, lon);
            assert!(
                (x - want_x).abs() < 1e-3 && (y - want_y).abs() < 1e-3,
                "LaD=45/Latin=30 forward ({lat}, {lon}): got ({x:.6}, {y:.6}), \
                 PROJ says ({want_x:.6}, {want_y:.6})",
            );
            let (rlat, rlon) = p.inverse(x, y);
            assert!(
                (rlat - lat).abs() < 1e-9 && (rlon - lon).abs() < 1e-9,
                "round trip ({lat}, {lon}) -> ({rlat}, {rlon})",
            );
        }
    }

    /// The secant formula must agree with the tangent limit as the parallels
    /// close up — i.e. the two branches are one continuous function, not two
    /// unrelated ones.
    ///
    /// Expected values come from `f64::sin` of the mean parallel, not from the
    /// projection.
    #[test]
    fn secant_cone_constant_approaches_the_tangent_limit() {
        for delta in [1.0_f64, 0.1, 0.01, 0.001] {
            let (lat1, lat2) = (38.5 - delta / 2.0, 38.5 + delta / 2.0);
            let secant =
                LambertConformalConic::new(6_371_229.0, 6_371_229.0, 38.5, 262.5, lat1, lat2)
                    .unwrap();
            // For a sphere the exact secant cone constant is
            // ln(cos φ1 / cos φ2) / ln(tan(π/4+φ2/2) / tan(π/4+φ1/2)); as
            // δ -> 0 this tends to sin(38.5°). The error is O(δ²), so a
            // δ²-scaled bound is the honest assertion here.
            let expected = 38.5_f64.to_radians().sin();
            let bound = 1e-3 * delta * delta;
            assert!(
                (secant.n - expected).abs() < bound,
                "secant n for parallels ±{}° = {}, tangent limit {expected}, \
                 difference exceeds the O(δ²) bound {bound}",
                delta / 2.0,
                secant.n,
            );
            // And it must genuinely take the secant branch, not silently fall
            // into the tangent one.
            assert_ne!(
                secant.n, expected,
                "delta={delta} was absorbed by the tangent epsilon",
            );
        }
    }

    /// Exchanging the two standard parallels must change nothing.
    ///
    /// This is recorded as a test because it is the one obvious "mutation" of
    /// this code that is *not* a bug: `n` (eq. 15-4) is antisymmetric in both
    /// its numerator and denominator under the swap, so it is unchanged, and
    /// `F` (eq. 15-2) is by construction identical whether computed from
    /// parallel 1 or parallel 2 — that equality is what defines `n`. So a test
    /// suite cannot distinguish the two orderings, and should not be expected
    /// to.
    ///
    /// PROJ 9.8.1 agrees: the EPSG example below evaluates to
    /// (293676.579980462, 77650.942538947) with the published parallel order
    /// and (293676.579980462, 77650.942538950) with them swapped — 3 nm apart,
    /// i.e. floating-point noise.
    #[test]
    fn exchanging_the_standard_parallels_is_a_no_op() {
        let a = 6_378_206.400;
        let b = a * (1.0 - 1.0 / 294.97870);
        let (lat0, lon0) = (27.0 + 50.0 / 60.0, -99.0);
        let (lat1, lat2) = (28.0 + 23.0 / 60.0, 30.0 + 17.0 / 60.0);

        let normal = LambertConformalConic::new(a, b, lat0, lon0, lat1, lat2).unwrap();
        let swapped = LambertConformalConic::new(a, b, lat0, lon0, lat2, lat1).unwrap();

        assert!((normal.n - swapped.n).abs() < 1e-15, "cone constant differs");
        assert!(
            (normal.big_f - swapped.big_f).abs() / normal.big_f < 1e-12,
            "scale factor differs: {} vs {}",
            normal.big_f,
            swapped.big_f,
        );
        for (lat, lon) in [(28.5, -96.0), (31.0, -102.0), (26.0, -95.0)] {
            let (x1, y1) = normal.forward(lat, lon);
            let (x2, y2) = swapped.forward(lat, lon);
            assert!(
                (x1 - x2).abs() < 1e-6 && (y1 - y2).abs() < 1e-6,
                "({lat}, {lon}): ({x1}, {y1}) vs ({x2}, {y2})",
            );
        }
    }

    /// **Source (C) — EPSG Guidance Note 7-2, method 9802
    /// "Lambert Conic Conformal (2SP)", published worked example.**
    ///
    /// This is the secant *and* ellipsoidal path, which HRRR never exercises
    /// (HRRR is tangent on a sphere) and which grib's own upstream test does
    /// not exercise either — its only Lambert fixture, `ds.critfireo.bin`, is
    /// also tangent, at Latin1 = Latin2 = 25°. Without this test the secant and
    /// ellipsoid branches would ship completely unverified.
    ///
    /// From the EPSG guidance note:
    ///
    /// ```text
    ///   Ellipsoid:  Clarke 1866   a = 6378206.400 m   1/f = 294.97870
    ///   Latitude of false origin        27°50'00"N  = 27.833333°
    ///   Longitude of false origin       99°00'00"W  = -99.0°
    ///   First  standard parallel        28°23'00"N  = 28.383333°
    ///   Second standard parallel        30°17'00"N  = 30.283333°
    ///   Easting  at false origin  2000000.00 US survey feet
    ///   Northing at false origin        0.00 US survey feet
    ///
    ///   Forward, for  latitude 28°30'00"N, longitude 96°00'00"W:
    ///     Easting  E = 2963503.91 US survey feet
    ///     Northing N =  254759.80 US survey feet
    /// ```
    ///
    /// This module has no false easting/northing (grib's PROJ string has none
    /// either), so the false origin offsets are applied here in the test.
    /// 1 US survey foot = 1200/3937 m exactly.
    ///
    /// Cross-check: PROJ 9.8.1 gives (293676.579980, 77650.942539) m for the
    /// same input, versus (293676.5791, 77650.9423) m from the published
    /// figures — agreement to under a millimetre, so sources (B) and (C)
    /// corroborate each other.
    #[test]
    fn secant_ellipsoidal_matches_the_epsg_published_worked_example() {
        const US_SURVEY_FOOT_M: f64 = 1200.0 / 3937.0;
        const FALSE_EASTING_USFT: f64 = 2_000_000.00;
        const FALSE_NORTHING_USFT: f64 = 0.00;

        // Clarke 1866, from the same guidance note.
        let a = 6_378_206.400;
        let inv_f = 294.97870;
        let b = a * (1.0 - 1.0 / inv_f);

        let p = LambertConformalConic::new(
            a,
            b,
            27.0 + 50.0 / 60.0, // 27°50'N
            -99.0,
            28.0 + 23.0 / 60.0, // 28°23'N
            30.0 + 17.0 / 60.0, // 30°17'N
        )
        .unwrap();

        // Forward.
        let (x, y) = p.forward(28.5, -96.0);
        let easting_usft = x / US_SURVEY_FOOT_M + FALSE_EASTING_USFT;
        let northing_usft = y / US_SURVEY_FOOT_M + FALSE_NORTHING_USFT;

        // The guidance note prints its expected coordinates to 0.01 US survey
        // feet (3 mm), so that rounding is the floor on agreement; 0.02 US
        // survey feet (6 mm) is the tightest honest bound.
        assert!(
            (easting_usft - 2_963_503.91).abs() < 0.02,
            "EPSG 9802 easting: got {easting_usft:.2} usft, expected 2963503.91",
        );
        assert!(
            (northing_usft - 254_759.80).abs() < 0.02,
            "EPSG 9802 northing: got {northing_usft:.2} usft, expected 254759.80",
        );

        // Inverse of the published coordinates must return the published
        // geodetic position. The note gives 6 significant decimals of a
        // degree; 1e-7° is ~11 mm, comfortably inside the 3 mm rounding of the
        // easting/northing it is derived from.
        let (lat, lon) = p.inverse(
            (2_963_503.91 - FALSE_EASTING_USFT) * US_SURVEY_FOOT_M,
            (254_759.80 - FALSE_NORTHING_USFT) * US_SURVEY_FOOT_M,
        );
        assert!(
            (lat - 28.5).abs() < 1e-7 && (lon - (-96.0)).abs() < 1e-7,
            "EPSG 9802 inverse: got ({lat:.9}, {lon:.9}), expected (28.5, -96.0)",
        );
    }

    /// The eccentricity must be load-bearing, not decorative.
    ///
    /// [`secant_ellipsoidal_matches_the_epsg_published_worked_example`] passes
    /// to 6 mm. That is only meaningful if a *spherical* projection with the
    /// same semi-major axis would visibly fail the same check — otherwise the
    /// ellipsoidal machinery in [`LambertConformalConic::t`],
    /// [`LambertConformalConic::m`] and the iterative branch of
    /// [`LambertConformalConic::inverse`] could all be dead code and nobody
    /// would know.
    ///
    /// Measured separation at the EPSG test point is ~332 m in northing; the
    /// 100 m bound below is a floor on that, not a fitted value.
    #[test]
    fn dropping_the_eccentricity_would_fail_the_epsg_example() {
        const US_SURVEY_FOOT_M: f64 = 1200.0 / 3937.0;
        let a = 6_378_206.400;
        let b = a * (1.0 - 1.0 / 294.97870);
        let (lat0, lon0, lat1, lat2) =
            (27.0 + 50.0 / 60.0, -99.0, 28.0 + 23.0 / 60.0, 30.0 + 17.0 / 60.0);

        let ellipsoid = LambertConformalConic::new(a, b, lat0, lon0, lat1, lat2).unwrap();
        let sphere = LambertConformalConic::new(a, a, lat0, lon0, lat1, lat2).unwrap();

        assert!(ellipsoid.e > 0.0, "Clarke 1866 must have non-zero eccentricity");
        assert_eq!(sphere.e, 0.0, "a == b must give exactly zero eccentricity");

        // Distance between the two answers at the EPSG test point.
        let (ex, ey) = ellipsoid.forward(28.5, -96.0);
        let (sx, sy) = sphere.forward(28.5, -96.0);
        let separation = ((ex - sx).powi(2) + (ey - sy).powi(2)).sqrt();
        assert!(
            separation > 100.0,
            "sphere and ellipsoid differ by only {separation} m at the EPSG \
             test point — the eccentricity is not reaching the projection",
        );

        // Concretely: the spherical answer misses the published northing by
        // far more than the 0.02 US survey foot the ellipsoidal one meets.
        let sphere_northing_usft = sy / US_SURVEY_FOOT_M;
        assert!(
            (sphere_northing_usft - 254_759.80).abs() > 100.0,
            "a spherical projection would still pass the EPSG check \
             (northing {sphere_northing_usft:.2} usft vs published 254759.80), \
             so that test proves nothing about the ellipsoidal path",
        );

        // And the ellipsoidal inverse must undo the ellipsoidal forward — the
        // iteration must converge, not merely run.
        let (lat, lon) = ellipsoid.inverse(ex, ey);
        assert!(
            (lat - 28.5).abs() < 1e-9 && (lon + 96.0).abs() < 1e-9,
            "ellipsoidal round trip drifted to ({lat}, {lon})",
        );
    }

    /// A southern-hemisphere cone gives `n < 0`, which flips the sign
    /// conventions in Snyder eq. 15-11 and 14-11. Untested, that path silently
    /// mirrors the grid.
    ///
    /// **Source (B) — PROJ 9.8.1**, forward direction:
    ///
    /// ```text
    /// $ proj -f "%.6f" +proj=lcc +a=6371229 +b=6371229 \
    ///       +lat_0=-38.5 +lon_0=145 +lat_1=-30 +lat_2=-60 <<EOF
    /// 150 -35
    /// 140 -45
    /// 170 -20
    /// EOF
    ///  446832.877539    366322.927131
    /// -379422.797189   -711785.038895
    /// 2719239.664947   1644373.250769
    /// ```
    ///
    /// Tolerance is 1 mm, which is the printed precision of the reference.
    #[test]
    fn southern_cone_matches_proj_and_round_trips() {
        let p =
            LambertConformalConic::new(6_371_229.0, 6_371_229.0, -38.5, 145.0, -30.0, -60.0).unwrap();
        assert!(p.n < 0.0, "southern parallels must give a negative cone constant");

        let from_proj: &[(f64, f64, f64, f64)] = &[
            (-35.0, 150.0, 446_832.877539, 366_322.927131),
            (-45.0, 140.0, -379_422.797189, -711_785.038895),
            (-20.0, 170.0, 2_719_239.664947, 1_644_373.250769),
        ];
        for &(lat, lon, want_x, want_y) in from_proj {
            let (x, y) = p.forward(lat, lon);
            assert!(
                (x - want_x).abs() < 1e-3 && (y - want_y).abs() < 1e-3,
                "southern cone forward ({lat}, {lon}): got ({x:.6}, {y:.6}), \
                 PROJ says ({want_x:.6}, {want_y:.6})",
            );
            let (rlat, rlon) = p.inverse(x, y);
            assert!(
                (rlat - lat).abs() < 1e-9 && (rlon - lon).abs() < 1e-9,
                "southern cone round trip ({lat}, {lon}) -> ({x}, {y}) -> ({rlat}, {rlon})",
            );
        }
    }

    /// Degenerate parameter sets must be rejected, not silently produce NaN
    /// grids that render as an empty overlay.
    #[test]
    fn degenerate_parameters_are_rejected() {
        // lat_1 == -lat_2: the cone opens into a cylinder, n == 0.
        assert!(
            LambertConformalConic::new(6_371_229.0, 6_371_229.0, 0.0, 0.0, -30.0, 30.0).is_err(),
            "parallels symmetric about the equator must be rejected",
        );
        // Standard parallel at a pole.
        assert!(
            LambertConformalConic::new(6_371_229.0, 6_371_229.0, 45.0, 0.0, 90.0, 45.0).is_err(),
            "a standard parallel at the pole must be rejected",
        );
        // Nonsense axes.
        assert!(LambertConformalConic::new(0.0, 0.0, 38.5, 0.0, 38.5, 38.5).is_err());
        assert!(
            LambertConformalConic::new(6_356_752.0, 6_378_137.0, 38.5, 0.0, 38.5, 38.5).is_err(),
            "semi-minor axis larger than semi-major must be rejected",
        );
        // Unknown earth-shape code must surface as an error from `latlons`,
        // not a panic.
        let mut grid = hrrr_conus_grid();
        grid.earth_shape.shape = 200;
        assert!(latlons(&grid).is_err(), "unknown Code Table 3.2 value must error");
    }

    /// Longitude must land in -180..180, agreeing with grib's
    /// `normalize_latlon` over the range either one can actually be handed.
    ///
    /// Not a bit-for-bit clone of grib: grib uses `(lon + 540) % 360 - 180`,
    /// where Rust's `%` is a remainder that keeps the sign of the dividend, so
    /// grib returns out-of-range values below -540° (grib maps -600° to -240°).
    /// This uses `rem_euclid`, which is a true modulus and maps -600° to +120°,
    /// the correct answer. The two agree everywhere above -540°, which covers
    /// every longitude a GRIB2 grid can encode — Lo1 and LoV are unsigned
    /// microdegrees, so the inputs are 0..360 before any arithmetic, and the
    /// inverse only ever adds `lon_0`. The divergence is therefore unreachable
    /// from `latlons`; the cases below stay inside the shared range.
    #[test]
    fn longitude_normalization_lands_in_the_expected_half_open_range() {
        for (input, expected) in [
            (-180.0, -180.0),
            (0.0, 0.0),
            (179.0, 179.0),
            (180.0, -180.0),
            (360.0, 0.0),
            (540.0, -180.0),
            (237.280472, -122.719528),
            (262.5, -97.5),
        ] {
            let got = normalize_longitude_degrees(input);
            assert!(
                (got - expected).abs() < 1e-9,
                "normalize({input}) = {got}, expected {expected}",
            );
        }
    }

    /// The scanning-mode sign rules must actually be applied. HRRR scans +i/+j,
    /// so flipping the bits must move the grid, and must move it the other way.
    #[test]
    fn scanning_mode_flips_the_grid_walk() {
        use grib::def::grib2::template::param_set::ScanningMode;

        let grid = hrrr_conus_grid();
        let forward = latlons(&grid).unwrap();

        // 0b1100_0000: -i (bit 7 set), +j.
        let mut flipped_i = hrrr_conus_grid();
        flipped_i.scanning_mode = ScanningMode(0b1100_0000);
        let west = latlons(&flipped_i).unwrap();

        // 0b0000_0000: +i, -j (bit 6 clear).
        let mut flipped_j = hrrr_conus_grid();
        flipped_j.scanning_mode = ScanningMode(0b0000_0000);
        let south = latlons(&flipped_j).unwrap();

        // Point (0,0) is La1/Lo1 in every case — it is the anchor.
        for set in [&forward, &west, &south] {
            assert_latlon_close(
                set[0],
                (21.138123, -122.719528),
                HRRR_TOLERANCE_DEG,
                "anchor point is La1/Lo1 regardless of scan direction",
            );
        }

        let i1 = index_of(&grid, 1, 0);
        let j1 = index_of(&grid, 0, 1);
        assert!(
            forward[i1].1 > forward[0].1 && west[i1].1 < west[0].1,
            "flipping the i scan bit must reverse the longitude step: \
             +i gave {} -> {}, -i gave {} -> {}",
            forward[0].1,
            forward[i1].1,
            west[0].1,
            west[i1].1,
        );
        assert!(
            forward[j1].0 > forward[0].0 && south[j1].0 < south[0].0,
            "flipping the j scan bit must reverse the latitude step: \
             +j gave {} -> {}, -j gave {} -> {}",
            forward[0].0,
            forward[j1].0,
            south[0].0,
            south[j1].0,
        );
    }
}
