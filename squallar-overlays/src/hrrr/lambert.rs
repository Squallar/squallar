//! Lambert Conformal Conic projection, in pure Rust.
//!
//! Reproduces what grib handed PROJ:
//!
//! ```text
//! +a=<a> +b=<b> +proj=lcc +lat_0=<LaD> +lon_0=<LoV> +lat_1=<Latin1> +lat_2=<Latin2>
//! ```
//!
//! Math is Snyder, *Map Projections — A Working Manual* (USGS PP 1395) ch. 15.
//! The ellipsoidal formulation is used throughout and reduces exactly to the
//! spherical one at zero eccentricity, which is HRRR's case.
//!
//! **HRRR is the TANGENT case** — Latin1 == Latin2 == 38.5° — where Snyder
//! eq. 15-4 for the cone constant is 0/0 and must be replaced by its limit
//! `n = sin(lat_1)`. The secant branch is implemented but unexercised by real
//! input.

use grib::{GridPointIndex, def::grib2::template::Template3_30};

/// Below this parallel separation the tangent limit `n = sin(lat_1)` is used.
const TANGENT_EPSILON_RAD: f64 = 1e-9;

/// ~6 micrometres on the ground.
const LATITUDE_ITERATION_TOLERANCE_RAD: f64 = 1e-12;

/// Terrestrial eccentricities converge in under ten passes; the cap only stops a
/// pathological ellipsoid hanging a fetch.
const MAX_LATITUDE_ITERATIONS: usize = 32;

/// Constructed from the same six parameters PROJ's `+proj=lcc` takes, so it can
/// be checked against PROJ directly.
#[derive(Debug, Clone, Copy, PartialEq)]
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
    /// * `a`, `b` — semi-major and semi-minor axes in metres; `a == b` gives a
    ///   sphere. **HRRR's GRIB2 section 3 carries earth-shape code 6**, which WMO
    ///   Code Table 3.2 defines as a sphere of radius **6,371,229.0 m** — not the
    ///   6,371,200 m of code 8 that most HRRR write-ups quote, and not an
    ///   ellipsoid. Getting it wrong displaces the whole grid by tens of metres
    ///   without looking wrong. Take the radii from `EarthShape::radii()`.
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
            // Tangent cone: Snyder eq. 15-4 is 0/0, limit is sin(lat_1). The only
            // branch HRRR takes.
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

    /// Snyder eq. 15-1: polar radius for a latitude, metres. Monotone in
    /// latitude — decreasing for `n > 0`, increasing for `n < 0` — so a
    /// latitude range's radii are bounded by its two endpoints either way.
    fn rho(&self, lat_deg: f64) -> f64 {
        self.a * self.big_f * Self::t(lat_deg.to_radians(), self.e).powf(self.n)
    }

    /// Snyder eq. 14-4: cone angle for a longitude, radians.
    fn theta(&self, lon_deg: f64) -> f64 {
        self.n * normalize_radians(lon_deg.to_radians() - self.lon0)
    }

    /// Snyder eq. 14-1/14-2: polar to plane.
    fn plane(&self, rho: f64, theta: f64) -> (f64, f64) {
        (rho * theta.sin(), self.rho0 - rho * theta.cos())
    }

    /// Project geodetic degrees to projection-plane metres.
    ///
    /// Snyder eq. 15-1, 15-2, 14-4. The origin of the returned coordinates is
    /// `(lon_0, lat_0)`; there is no false easting/northing, matching the PROJ
    /// string grib builds.
    pub fn forward(&self, lat_deg: f64, lon_deg: f64) -> (f64, f64) {
        self.plane(self.rho(lat_deg), self.theta(lon_deg))
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
                    - 2.0
                        * (t * ((1.0 - self.e * s) / (1.0 + self.e * s)).powf(self.e / 2.0)).atan();
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

/// Wrap radians into `-pi..=pi`. `theta` must be the *short* way round or the
/// forward projection lands in the wrong quadrant for a grid straddling the
/// antimeridian. (GRIB2 stores LoV/Lo1 as unsigned microdegrees; HRRR's are
/// 262500000 and 237280472.)
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

/// A template 3.30 grid reduced to the constants any point's lat/lon can be
/// rebuilt from — 104 bytes standing in for two 15 MB `Vec<f64>`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LambertGrid {
    projection: LambertConformalConic,
    /// Projection-plane metres of grid point (0, 0).
    x0: f64,
    y0: f64,
    /// Signed step per index — the encoding is unsigned, the scanning mode
    /// carries the direction.
    dx: f64,
    dy: f64,
    ni: usize,
    nj: usize,
    /// Scanning-mode bits that decide the flat index order.
    i_consecutive: bool,
    alternating: bool,
    /// Whether adjacent grid points can jump in longitude — see
    /// [`Self::wraps_longitude`]. Measured once here, not per use.
    wraps_longitude: bool,
}

/// A [`LambertGrid`]'s stored constants as one plain struct of public fields —
/// its wire form, produced by [`LambertGrid::to_parts`] and consumed by
/// [`LambertGrid::from_parts`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LambertGridParts {
    pub a: f64,
    pub e: f64,
    pub n: f64,
    /// Scale factor (Snyder eq. 15-2).
    pub big_f: f64,
    pub rho0: f64,
    pub lon0: f64,
    pub x0: f64,
    pub y0: f64,
    pub dx: f64,
    pub dy: f64,
    pub ni: usize,
    pub nj: usize,
    pub i_consecutive: bool,
    pub alternating: bool,
    pub wraps_longitude: bool,
}

impl LambertGrid {
    /// Template 3.30 fixes the angular unit at 1e-6 degrees; there is no basic
    /// angle / subdivision pair in this template to override it.
    const ANGLE_UNIT: f64 = 1e-6;
    /// Dx and Dy are in millimetres for this template.
    const LENGTH_UNIT_M: f64 = 1e-3;

    /// Read the projection and grid walk out of GRIB2 section 3.
    pub fn from_template(grid: &Template3_30) -> Result<Self, String> {
        let lad = f64::from(grid.lad) * Self::ANGLE_UNIT;
        let lov = f64::from(grid.lov) * Self::ANGLE_UNIT;
        let latin1 = f64::from(grid.latin1) * Self::ANGLE_UNIT;
        let latin2 = f64::from(grid.latin2) * Self::ANGLE_UNIT;

        let (a, b) = grid.earth_shape.radii().ok_or_else(|| {
            format!(
                "Unknown value of GRIB2 Code Table 3.2 (shape of the Earth): {}",
                grid.earth_shape.shape
            )
        })?;

        let projection = LambertConformalConic::new(a, b, lad, lov, latin1, latin2)?;

        // Constructed and dropped for its flag validation: `has_unsupported_flags`
        // is private to grib, and rejecting exactly what grib's own iterator
        // rejects is the point.
        grid.ij()
            .map_err(|e| format!("Cannot iterate Lambert grid indices: {e}"))?;

        // Grid steps are always positive in the encoding; the scanning mode says
        // which way they actually run.
        let mut dx = f64::from(grid.dx) * Self::LENGTH_UNIT_M;
        let mut dy = f64::from(grid.dy) * Self::LENGTH_UNIT_M;
        if !grid.scanning_mode.scans_positively_for_i() && dx > 0.0 {
            dx = -dx;
        }
        if !grid.scanning_mode.scans_positively_for_j() && dy > 0.0 {
            dy = -dy;
        }

        let (x0, y0) = projection.forward(
            f64::from(grid.first_point_lat) * Self::ANGLE_UNIT,
            f64::from(grid.first_point_lon) * Self::ANGLE_UNIT,
        );

        let mut geometry = Self {
            projection,
            x0,
            y0,
            dx,
            dy,
            ni: grid.ni as usize,
            nj: grid.nj as usize,
            i_consecutive: grid.scanning_mode.is_consecutive_for_i(),
            alternating: grid.scanning_mode.scans_alternating_rows(),
            wraps_longitude: false,
        };
        geometry.wraps_longitude = geometry.detect_longitude_wrap();
        Ok(geometry)
    }

    /// Whether two *adjacent* grid points can differ by more than half a turn
    /// in longitude.
    ///
    ///  * [`normalize_longitude_degrees`] folds at ±180, so a grid straddling
    ///    the anti-meridian has neighbours a whole turn apart;
    ///  * [`LambertConformalConic::inverse`] takes `atan2`, whose own cut at
    ///    ±pi is the *cone's* seam. Crossing it moves longitude by `360 / n`
    ///    degrees — 578 for HRRR — which normalises to a 218° jump.
    fn detect_longitude_wrap(&self) -> bool {
        if self.ni == 0 || self.nj == 0 {
            return false;
        }
        // The boundary alone is enough, for a different reason per cut.
        // The band cut is a *crossing* test on a meridian, which this projection
        // draws as a straight ray from the apex: a ray cannot cross the grid
        // without crossing an edge between two adjacent boundary points.
        for j in [0, self.nj - 1] {
            for i in 1..self.ni {
                if self.step_is_discontinuous((i - 1, j), (i, j)) {
                    return true;
                }
            }
        }
        for i in [0, self.ni - 1] {
            for j in 1..self.nj {
                if self.step_is_discontinuous((i, j - 1), (i, j)) {
                    return true;
                }
            }
        }
        false
    }

    /// The longitude [`LambertConformalConic::inverse`] reports for grid point
    /// `(i, j)`, before [`normalize_longitude_degrees`] folds it.
    fn raw_lon(&self, i: usize, j: usize) -> f64 {
        self.projection
            .inverse(self.x0 + self.dx * i as f64, self.y0 + self.dy * j as f64)
            .1
    }

    /// Whether the longitudes of these two grid points are separated by one of
    /// the two cuts, rather than by ground.
    ///
    /// Both are located exactly rather than inferred from the size of the jump.
    /// Size is not a reliable signal: crossing the cone's seam moves longitude
    /// by `360 / n` — 578° for HRRR — which lands 218° away but can normalise to
    /// its 141.7° complement, *under* any half-turn threshold.
    fn step_is_discontinuous(&self, a: (usize, usize), b: (usize, usize)) -> bool {
        let (raw_a, raw_b) = (self.raw_lon(a.0, a.1), self.raw_lon(b.0, b.1));

        // Cut 1 — the cone's seam. `inverse` reads the angle with `atan2`, so
        // the longitudes it can report span `360 / n`: 578° for HRRR, i.e. more
        // than a turn. Longitude is therefore *not* single-valued over the
        // plane, and `theta` — which folds into one turn about `lon0` before
        // scaling — only ever names the principal preimage. A grid outside that
        // sector is described by the other one, and every longitude-to-index
        // answer for it is computed on the wrong arc. The sector is exactly
        // `|raw - lon0| <= 180`.
        let lon0 = self.projection.lon0.to_degrees();
        if (raw_a - lon0).abs() > 180.0 || (raw_b - lon0).abs() > 180.0 {
            return true;
        }

        // Cut 2 — the anti-meridian. `normalize_longitude_degrees` folds at
        // every odd multiple of 180, so a step across one lands a turn away in
        // the coordinates the rasterizer actually projects.
        let band = |lon: f64| ((lon + 180.0) / 360.0).floor();
        band(raw_a) != band(raw_b)
    }

    /// See [`Self::detect_longitude_wrap`]. Free to call; measured at
    /// construction.
    pub fn wraps_longitude(&self) -> bool {
        self.wraps_longitude
    }

    /// This grid's stored constants, whole, for the one boundary that can only
    /// carry numbers: `squallar_worker::offload`'s described-overlay codec.
    pub fn to_parts(&self) -> LambertGridParts {
        LambertGridParts {
            a: self.projection.a,
            e: self.projection.e,
            n: self.projection.n,
            big_f: self.projection.big_f,
            rho0: self.projection.rho0,
            lon0: self.projection.lon0,
            x0: self.x0,
            y0: self.y0,
            dx: self.dx,
            dy: self.dy,
            ni: self.ni,
            nj: self.nj,
            i_consecutive: self.i_consecutive,
            alternating: self.alternating,
            wraps_longitude: self.wraps_longitude,
        }
    }

    /// The inverse of [`Self::to_parts`]: a grid from stored constants, or
    /// `None` for constants no [`Self::from_template`] construction ever
    /// produces — a non-finite number anywhere, a non-positive semi-major
    /// axis, an eccentricity outside `[0, 1)`, or a zero cone constant, each
    /// of which would make the projection arithmetic answer NaN for every
    /// point rather than fail.
    pub fn from_parts(parts: LambertGridParts) -> Option<Self> {
        let finite = [
            parts.a,
            parts.e,
            parts.n,
            parts.big_f,
            parts.rho0,
            parts.lon0,
            parts.x0,
            parts.y0,
            parts.dx,
            parts.dy,
        ]
        .iter()
        .all(|v| v.is_finite());
        if !finite
            || parts.a <= 0.0
            || !(0.0..1.0).contains(&parts.e)
            || parts.n == 0.0
            || parts.big_f == 0.0
        {
            return None;
        }
        Some(Self {
            projection: LambertConformalConic {
                a: parts.a,
                e: parts.e,
                n: parts.n,
                big_f: parts.big_f,
                rho0: parts.rho0,
                lon0: parts.lon0,
            },
            x0: parts.x0,
            y0: parts.y0,
            dx: parts.dx,
            dy: parts.dy,
            ni: parts.ni,
            nj: parts.nj,
            i_consecutive: parts.i_consecutive,
            alternating: parts.alternating,
            wraps_longitude: parts.wraps_longitude,
        })
    }

    /// Number of grid points.
    pub fn len(&self) -> usize {
        self.ni * self.nj
    }

    /// A grid with no points; only reachable from a malformed section 3.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Lat/lon of grid point `(i, j)`, degrees, longitude wrapped to -180..180.
    pub fn latlon(&self, i: usize, j: usize) -> (f64, f64) {
        let (lat, lon) = self
            .projection
            .inverse(self.x0 + self.dx * i as f64, self.y0 + self.dy * j as f64);
        (lat, normalize_longitude_degrees(lon))
    }

    /// Lat/lon of the `index`-th decoded value, or `None` past the end.
    pub fn latlon_at(&self, index: usize) -> Option<(f64, f64)> {
        let (i, j) = self.ij_at(index)?;
        Some(self.latlon(i, j))
    }

    /// Flat index of the grid point nearest `(lat, lon)`, or `None` when the
    /// point falls outside the grid.
    ///
    /// Forward-projecting and dividing by the step is exact for this grid — the
    /// axes are the projection's own — so this replaces a scan over every point.
    pub fn nearest(&self, lat: f64, lon: f64) -> Option<usize> {
        let (x, y) = self.projection.forward(lat, lon);
        let fi = ((x - self.x0) / self.dx).round();
        let fj = ((y - self.y0) / self.dy).round();
        if !(fi.is_finite() && fj.is_finite()) || fi < 0.0 || fj < 0.0 {
            return None;
        }
        let (i, j) = (fi as usize, fj as usize);
        if i >= self.ni || j >= self.nj {
            return None;
        }
        Some(self.index_of(i, j))
    }

    /// Whether the flat scan index is exactly `j * ni + i` over an `ni` x `nj`
    /// grid — what a caller stepping to `index ± 1` / `index ± ni` for the four
    /// neighbours is assuming. False for the other fifteen scanning modes.
    pub(crate) fn is_row_major(&self, ni: usize, nj: usize) -> bool {
        self.i_consecutive && !self.alternating && self.ni == ni && self.nj == nj
    }

    /// Whether `min_lon..max_lon` crosses the cone's **seam** — the meridian
    /// opposite the central one, `lon0 + 180`.
    ///
    /// [`LambertConformalConic::theta`] folds `lon - lon0` into -pi..=pi and
    /// *then* multiplies by the cone constant, so crossing that meridian moves
    /// the plane point by `n * 2pi` — for HRRR 224°, not a whole turn. The image
    /// of a longitude range spanning it is therefore two disjoint arcs, and
    /// anything that treats it as one interval is describing a different region
    /// of the plane than the one it was asked about.
    pub fn crosses_seam(&self, min_lon: f64, max_lon: f64) -> bool {
        let p = &self.projection;
        let span = p.n * (max_lon - min_lon).to_radians();
        !span.is_finite()
            || span.abs() >= std::f64::consts::TAU
            || (p.theta(min_lon) + span - p.theta(max_lon)).abs() > 1e-9
    }

    /// Upper bound on how many degrees one grid cell spans near `lat`, along
    /// either axis.
    ///
    /// A step of `s` metres is at most `s / (a cos φ)` radians of longitude and
    /// exactly `s / a` radians of latitude, and the longitude term is the larger
    /// away from the equator — so one number bounds both, and (divided by
    /// `cos φ` once more, i.e. the same number in radians) also bounds the step
    /// in Mercator `y`. That is what lets a caller state a margin in *cells*
    /// rather than as a fraction of whatever box it happens to be rendering.
    pub fn cell_span_degrees(&self, lat: f64) -> f64 {
        // Not the pole: `cos` there is zero and the bound is unbounded, which is
        // true but useless. 89.9° already gives ~570x the equatorial cell.
        let cos = lat.abs().min(89.9).to_radians().cos();
        let step = self.dx.abs().max(self.dy.abs());
        (step / (self.projection.a * cos)).to_degrees()
    }

    /// Fractional `(i_min, i_max, j_min, j_max)` bounding every grid point
    /// inside the lat/lon box, or `None` when no useful bound exists.
    ///
    /// **Exact, not sampled.** An LCC lat/lon box is an annular sector in the
    /// projection plane: `rho` depends only on latitude and `theta` only on
    /// longitude. So `x = rho·sin θ` and `y = rho0 − rho·cos θ` are extremal
    /// either at a corner of the sector or where `sin`/`cos` turns — a quadrant
    /// boundary of `theta` — and that candidate set is finite.
    ///
    /// Callers must still widen the result: a grid point just outside the box
    /// can influence a pixel inside it.
    pub fn index_bounds(
        &self,
        min_lat: f64,
        max_lat: f64,
        min_lon: f64,
        max_lon: f64,
    ) -> Option<(f64, f64, f64, f64)> {
        if min_lat > max_lat || min_lon > max_lon || self.dx == 0.0 || self.dy == 0.0 {
            return None;
        }
        let p = &self.projection;
        let rhos = [p.rho(min_lat), p.rho(max_lat)];

        // The theta interval, unwrapped: `theta` folds `lon - lon0` into
        // -pi..=pi, so stepping from one end is the only way to get a single
        // interval rather than two arcs — and it is only a single interval at
        // all if the box stays off the seam.
        if self.crosses_seam(min_lon, max_lon) {
            return None;
        }
        let span = p.n * (max_lon - min_lon).to_radians();
        let start = p.theta(min_lon);
        let (lo, hi) = (start.min(start + span), start.max(start + span));

        let quarter = std::f64::consts::FRAC_PI_2;
        let mut thetas = vec![lo, hi];
        let mut k = (lo / quarter).ceil();
        while k * quarter <= hi {
            thetas.push(k * quarter);
            k += 1.0;
        }

        let (mut x_min, mut x_max) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut y_min, mut y_max) = (f64::INFINITY, f64::NEG_INFINITY);
        for rho in rhos {
            for &theta in &thetas {
                let (x, y) = p.plane(rho, theta);
                x_min = x_min.min(x);
                x_max = x_max.max(x);
                y_min = y_min.min(y);
                y_max = y_max.max(y);
            }
        }

        // A negative step reverses the ordering, hence the min/max pairs.
        let (ia, ib) = ((x_min - self.x0) / self.dx, (x_max - self.x0) / self.dx);
        let (ja, jb) = ((y_min - self.y0) / self.dy, (y_max - self.y0) / self.dy);
        let out = (ia.min(ib), ia.max(ib), ja.min(jb), ja.max(jb));
        if !(out.0.is_finite() && out.1.is_finite() && out.2.is_finite() && out.3.is_finite()) {
            return None;
        }
        Some(out)
    }

    /// The `(i, j)` grib's [`GridPointIndex::ij`] yields at position `index`.
    fn ij_at(&self, index: usize) -> Option<(usize, usize)> {
        let (major_len, minor_len) = self.scan_lengths();
        if minor_len == 0 {
            return None;
        }
        let major = index / minor_len;
        if major >= major_len {
            return None;
        }
        let mut minor = index % minor_len;
        if self.alternating && major % 2 == 1 {
            minor = minor_len - minor - 1;
        }
        Some(if self.i_consecutive {
            (minor, major)
        } else {
            (major, minor)
        })
    }

    /// Inverse of [`Self::ij_at`].
    fn index_of(&self, i: usize, j: usize) -> usize {
        let (_, minor_len) = self.scan_lengths();
        let (major, minor) = if self.i_consecutive { (j, i) } else { (i, j) };
        let minor = if self.alternating && major % 2 == 1 {
            minor_len - minor - 1
        } else {
            minor
        };
        major * minor_len + minor
    }

    /// `(outer, inner)` loop lengths of the scan, in grib's own terms.
    fn scan_lengths(&self) -> (usize, usize) {
        if self.i_consecutive {
            (self.nj, self.ni)
        } else {
            (self.ni, self.nj)
        }
    }
}

/// Drop-in replacement for grib's PROJ-backed
/// `<Template3_30 as LatLons>::latlons`, mirroring its sequence exactly.
///
/// Points come back in scanning-mode order — the same order as the decoded data
/// values — because the iteration is driven by grib's own
/// [`GridPointIndex::ij`], which is not gated behind `gridpoints-proj`.
pub fn latlons(grid: &Template3_30) -> Result<Vec<(f64, f64)>, String> {
    let geometry = LambertGrid::from_template(grid)?;
    let indices = grid
        .ij()
        .map_err(|e| format!("Cannot iterate Lambert grid indices: {e}"))?;
    Ok(indices.map(|(i, j)| geometry.latlon(i, j)).collect())
}

/// HRRR CONUS grid definition, transcribed field-for-field from GRIB2 section 3
/// of a **raw operational** file (`hrrr.t12z.wrfsfcf00.grib2`, 2026-07-24 12Z),
/// fetched with no `subregion` so NOMADS streams it untouched. Matches
/// `wgrib2 -grid` and NOAA's published domain: 1799x1059, 3 km, LoV 262.5,
/// LaD/Latin1/Latin2 38.5.
///
/// A NOMADS filter-CGI download gives a different `Lo1` and the corner
/// assertions fail by a hair — see
/// [`tests::the_nomads_filter_reencodes_lo1_by_one_microdegree`]. Both are
/// correct; the projection anchors on whatever `Lo1` the file states.
#[cfg(test)]
pub(crate) fn hrrr_conus_grid() -> Template3_30 {
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

#[cfg(test)]
mod tests;
