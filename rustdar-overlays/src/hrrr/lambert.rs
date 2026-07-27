//! Lambert Conformal Conic projection, in pure Rust.
//!
//! `grib` can do this for GRIB2 template 3.30, but only behind `gridpoints-proj`,
//! which links PROJ (`proj-sys`, `libsqlite3-sys`, `link-cplusplus` — none of
//! which cross-compile to wasm32 or iOS). With `default-features = false`,
//! `Template3_30::latlons()` falls into grib's catch-all and returns
//! `GribError::NotSupported`. HRRR is *entirely* template 3.30, so that kills
//! every HRRR fetch.
//!
//! Reproduces what grib handed PROJ:
//!
//! ```text
//! +a=<a> +b=<b> +proj=lcc +lat_0=<LaD> +lon_0=<LoV> +lat_1=<Latin1> +lat_2=<Latin2>
//! ```
//!
//! and the same sequence: forward-project the grid's first point (La1/Lo1), step
//! by Dx/Dy in metres, inverse-project each step. That is why both directions are
//! implemented, not just the inverse.
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
///
/// GRIB2 stores Latin1/Latin2 as integer microdegrees, so the smallest non-zero
/// separation representable is 1e-6° = 1.745e-8 rad — an order of magnitude
/// above this, so a genuinely secant grid is never mistaken for a tangent one.
const TANGENT_EPSILON_RAD: f64 = 1e-9;

/// ~6 micrometres on the ground.
const LATITUDE_ITERATION_TOLERANCE_RAD: f64 = 1e-12;

/// Terrestrial eccentricities converge in under ten passes; the cap only stops a
/// pathological ellipsoid hanging a fetch.
const MAX_LATITUDE_ITERATIONS: usize = 32;

/// Constructed from the same six parameters PROJ's `+proj=lcc` takes, so it can
/// be checked against PROJ directly.
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
/// rebuilt from — 88 bytes standing in for two 15 MB `Vec<f64>`.
///
/// HRRR CONUS is 1,905,141 points, so materialising the coordinates costs
/// 30.5 MB per cached parameter and 61 MB at the peak of a parse (the pair
/// vector and the split arrays are alive together). Nothing downstream reads
/// them in bulk: the rasterizer walks the grid in index order and the tooltip
/// wants one point, both of which this answers arithmetically.
#[derive(Debug, Clone, Copy)]
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
    /// Two unrelated discontinuities show up as the same jump, and both break
    /// any caller that treats a cell as covering the ground between itself and
    /// its neighbour:
    ///
    ///  * [`normalize_longitude_degrees`] folds at ±180, so a grid straddling
    ///    the anti-meridian has neighbours a whole turn apart;
    ///  * [`LambertConformalConic::inverse`] takes `atan2`, whose own cut at
    ///    ±pi is the *cone's* seam. Crossing it moves longitude by `360 / n`
    ///    degrees — 578 for HRRR — which normalises to a 218° jump.
    ///
    /// Either way a Mercator rasterizer puts the two neighbours most of a
    /// texture apart and stretches the cell between them across the image.
    fn detect_longitude_wrap(&self) -> bool {
        if self.ni == 0 || self.nj == 0 {
            return false;
        }
        // The boundary alone is enough, for a different reason per cut.
        //
        // The band cut is a *crossing* test on a meridian, which this projection
        // draws as a straight ray from the apex: a ray cannot cross the grid
        // without crossing an edge between two adjacent boundary points.
        //
        // The sector cut is a *membership* test on a wedge (`|theta| > n*pi`,
        // angular width `2(1-n)pi`), not on its boundary rays. It reduces to the
        // boundary because the wedge is symmetric about the plane's y-axis with
        // its apex on it, and a template 3.30 grid is axis-aligned in that same
        // plane — so the wedge's horizontal slices nest in y, and any column bad
        // at some row is bad at j = 0 or j = nj-1. Both are walked.
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
    ///
    /// Deliberately not "does the grid's `min_lon..max_lon` contain the seam":
    /// for a grid that wraps, those two are the extreme *normalised*
    /// longitudes, so the discontinuity falls in the unpopulated gap between
    /// `max_lon` and `min_lon + 360` — outside the interval — and the test
    /// silently answers `false`. That is the same "interval predicate applied
    /// to something that is not an interval" mistake one level up, and it hides
    /// exactly the `LoV = 0` case, where the cone seam and the anti-meridian
    /// coincide.
    pub fn wraps_longitude(&self) -> bool {
        self.wraps_longitude
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
    /// "Nearest" is therefore in the projection plane rather than in degrees;
    /// on a 3 km grid the two can differ only for a query already sitting on a
    /// cell boundary.
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
    ///
    /// Detected by consistency rather than by comparing meridians: stepping from
    /// one end must land on the other end's own angle.
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
    ///
    /// Upper bound only where the scale factor `k >= 1`, which a tangent cone
    /// guarantees. A secant cone has `k < 1` between its parallels and this
    /// underestimates by `1/k` — ~3.5% for a 30/60 pair at 45°, inside
    /// `CELL_REACH`'s 0.75-against-0.55 headroom, but not by an unlimited
    /// margin. HRRR is tangent; revisit if a secant model is ever added.
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
    /// boundary of `theta` — and that candidate set is finite. Sampling the
    /// box's edges instead would be an approximation whose error is a function
    /// of the arc, and this is used to *skip* work, so an underestimate paints
    /// the wrong picture.
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
        // all if the box stays off the seam. Rather than bound two arcs,
        // decline; such a box spans half the planet, so there was little to
        // exclude anyway.
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
    ///
    /// Reproduces `GridPointIndexIterator` in closed form;
    /// `the_flat_index_mapping_reproduces_gribs_scan_order` pins it against the
    /// iterator itself rather than against this reasoning.
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
///
/// The fetch path no longer calls this; it keeps a [`LambertGrid`] and computes
/// points on demand. It survives as the reference the lazy form is checked
/// against, and as the eager form for any caller that genuinely wants all
/// 30 MB.
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
///
/// Lives outside `mod tests` so the rasterizer's tests can raster a grid with
/// the real HRRR geometry rather than a toy one.
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
mod tests {
    use super::*;

    // Nothing below is computed with the code under test. Three sources:
    //   (A) the HRRR GRIB2 file's own section 3 (La1/Lo1 is grid point (0,0));
    //   (B) PROJ 9.8.1, the C implementation grib's `gridpoints-proj` delegated
    //       to and therefore the behaviour this module must preserve;
    //   (C) the EPSG Guidance Note 7-2 worked example for "Lambert Conic
    //       Conformal (2SP)" (method 9802).

    /// <= 11 cm anywhere on this grid: 1e-6° of latitude is 0.111 m at every
    /// latitude (1° ≈ 111.2 km), while 1e-6° of longitude (1° ≈ 111.32·cos φ km)
    /// runs 0.104 m at 21°N down to 0.075 m at 48°N. The latitude term
    /// therefore sets the bound, and it is constant — even at the equator, where
    /// the longitude term peaks at 0.1113 m, the worst case moves by 0.12 mm. So
    /// this holds worldwide and needs no re-derivation for another domain.
    ///
    /// Deliberately far tighter than the 3 km spacing: the failure mode guarded
    /// against is systematic displacement (wrong earth radius, dropped central
    /// meridian), which is tens of metres at minimum.
    const HRRR_TOLERANCE_DEG: f64 = 1e-6;

    #[track_caller]
    fn assert_latlon_close(actual: (f64, f64), expected: (f64, f64), tol: f64, what: &str) {
        let (dlat, dlon) = ((actual.0 - expected.0).abs(), (actual.1 - expected.1).abs());
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

    /// **Source (A) — the file's own section 3.** No external tool in the loop.
    ///
    /// Not sufficient alone: `latlons` forward-projects La1/Lo1 and immediately
    /// inverse-projects it, so corner (0,0) is algebraically invariant under any
    /// change to the earth radius. Hence the far-corner tests below.
    #[test]
    fn first_grid_point_reproduces_the_files_own_la1_lo1() {
        let grid = hrrr_conus_grid();
        let points = latlons(&grid).unwrap();
        assert_eq!(points.len(), 1799 * 1059);
        assert_latlon_close(
            points[0],
            // Section 3: La1 = 21138123, Lo1 = 237280472 microdegrees.
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
        (
            1798,
            1058,
            47.842195_02,
            -60.917192_77,
            "NE corner (ni-1,nj-1)",
        ),
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

    /// The `Lo1` the retired NOMADS filter path produced, which also projects
    /// correctly.
    ///
    /// That request always carried `subregion=&toplat=...&leftlon=...`. Those do
    /// not subset a Lambert grid — the response was always the full 1799x1059
    /// CONUS field — but they were not inert: NOMADS re-encoded the record
    /// through wgrib2 rather than streaming it, re-rounding `Lo1` from 237280472
    /// to **237280471** microdegrees and re-packing DRT 5.3 to DRT 5.0. S3 serves
    /// the operational bytes, so the live decode path sees 5.3. grib handles both
    /// in pure Rust, which is why dropping JPEG2000 and CCSDS is safe either way.
    ///
    /// One microdegree of `Lo1` is ~10 cm at the anchor but rotates the grid
    /// about the cone apex, so at the NE corner the two encodings differ by
    /// 1.12e-6° — just over [`HRRR_TOLERANCE_DEG`]. Hence a separate fixture: a
    /// tolerance wide enough for both would hide a real error.
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

        // The two encodings really do disagree by more than the shared tolerance
        // at the far corner, so the fixtures cannot be swapped silently.
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

    // ── The lazy geometry ─────────────────────────────────────────────────

    /// [`LambertGrid::ij_at`] reproduces grib's `GridPointIndexIterator` in
    /// closed form, which is what lets a point be answered without walking the
    /// grid. Pinned against that iterator across **all sixteen** supported
    /// scanning modes rather than against the reasoning behind it — HRRR only
    /// ever sends 0b0100_0000, so the other fifteen have no other guard.
    #[test]
    fn the_flat_index_mapping_reproduces_gribs_scan_order() {
        use grib::def::grib2::template::param_set;
        let mut grid = hrrr_conus_grid();
        // Deliberately not square and not even, so a transposed or
        // off-by-one-row mapping cannot coincide with the right one.
        grid.ni = 5;
        grid.nj = 3;

        for high_nibble in 0..16u8 {
            let bits = high_nibble << 4;
            grid.scanning_mode = param_set::ScanningMode(bits);
            let expected: Vec<(usize, usize)> = grid.ij().unwrap().collect();
            let geometry = LambertGrid::from_template(&grid).unwrap();

            assert_eq!(expected.len(), geometry.len(), "mode {bits:#010b}");
            for (k, &(i, j)) in expected.iter().enumerate() {
                assert_eq!(
                    geometry.ij_at(k),
                    Some((i, j)),
                    "mode {bits:#010b}: index {k}",
                );
                assert_eq!(
                    geometry.index_of(i, j),
                    k,
                    "mode {bits:#010b}: point ({i}, {j})",
                );
            }
            assert_eq!(
                geometry.ij_at(expected.len()),
                None,
                "mode {bits:#010b}: one past the end must not wrap",
            );
        }
    }

    /// The lazy form must be **bit-identical** to the eager one it replaced, at
    /// every one of the 1,905,141 points — not close, identical: it is the same
    /// arithmetic, and every PROJ-anchored assertion above is stated in terms of
    /// [`latlons`].
    #[test]
    fn the_lazy_geometry_reproduces_the_eager_latlons() {
        let grid = hrrr_conus_grid();
        let eager = latlons(&grid).unwrap();
        let lazy = LambertGrid::from_template(&grid).unwrap();

        assert_eq!(lazy.len(), eager.len());
        for (k, &expected) in eager.iter().enumerate() {
            assert_eq!(lazy.latlon_at(k), Some(expected), "grid point {k}");
        }
        assert_eq!(lazy.latlon_at(eager.len()), None);
    }

    /// The tooltip's O(1) lookup must land where a scan over all 1.9 M points
    /// would.
    ///
    /// Sampled away from the anchor on purpose: `nearest` divides by the same
    /// `dx`/`dy` the forward projection was anchored with, so grid point (0, 0)
    /// resolves correctly even under a wrong step — the same blind-oracle shape
    /// as the earth-radius corner case above.
    #[test]
    fn nearest_recovers_the_index_of_a_grid_point() {
        let grid = hrrr_conus_grid();
        let geometry = LambertGrid::from_template(&grid).unwrap();

        for &(i, j, _, _, what) in HRRR_CORNERS_FROM_PROJ {
            let (lat, lon) = geometry.latlon(i, j);
            assert_eq!(
                geometry.nearest(lat, lon),
                Some(geometry.index_of(i, j)),
                "{what}",
            );
        }

        // A third of a cell off-centre still rounds to the same point.
        let (lat, lon) = geometry.latlon(900, 530);
        let (nlat, nlon) = geometry.latlon(901, 531);
        assert_eq!(
            geometry.nearest(lat + (nlat - lat) / 3.0, lon + (nlon - lon) / 3.0),
            Some(geometry.index_of(900, 530)),
            "a point inside a cell must resolve to that cell",
        );
    }

    /// One cell past each edge, not a continent away.
    ///
    /// The far-away cases below cannot see this: relaxing the upper bound to
    /// `i > ni` admits `i == ni`, and under i-consecutive scanning
    /// `index_of(ni, j)` is the *west* edge of row `j + 1` — an in-range index
    /// holding a reading from 5,000 km away, which `values.get` will happily
    /// return.
    #[test]
    fn nearest_refuses_the_cell_just_past_each_edge() {
        let grid = hrrr_conus_grid();
        let (ni, nj) = (grid.ni as usize, grid.nj as usize);
        let geometry = LambertGrid::from_template(&grid).unwrap();

        for &(i, j, what) in &[
            (ni, 0usize, "one column past the east edge"),
            (0usize, nj, "one row past the north edge"),
            (ni, nj, "past both edges"),
        ] {
            let (lat, lon) = geometry.latlon(i, j);
            assert_eq!(geometry.nearest(lat, lon), None, "{what}");
        }

        // Control: the last real point must still resolve, or the guard is
        // simply off by one the other way.
        let (lat, lon) = geometry.latlon(ni - 1, nj - 1);
        assert_eq!(
            geometry.nearest(lat, lon),
            Some(geometry.index_of(ni - 1, nj - 1)),
            "the far corner is inside the grid",
        );
    }

    /// [`LambertGrid::index_bounds`] is a closed form over a candidate set, not
    /// a scan, so it is checked against the scan it replaces: every grid point
    /// inside the box must fall inside the bounds it returns.
    ///
    /// The second half is what stops it degenerating into `0..ni`, which would
    /// satisfy the first half for ever: the answer must also be *tight*, within
    /// a cell of the true extent of the contained points.
    #[test]
    fn index_bounds_brackets_exactly_the_points_inside_the_box() {
        let mut template = hrrr_conus_grid();
        template.ni = 120;
        template.nj = 90;
        let g = LambertGrid::from_template(&template).unwrap();

        // Spread over the grid, including boxes that straddle an edge, cover
        // everything and miss entirely. Degrees, absolute. `tight` is off for a
        // box that runs off the grid: it is then legitimately wider than any
        // point it contains, and only containment is meaningful.
        let (lat0, lon0) = g.latlon(60, 45);
        let boxes: &[(f64, f64, f64, f64, bool, &str)] = &[
            (
                lat0 - 0.2,
                lat0 + 0.2,
                lon0 - 0.2,
                lon0 + 0.2,
                true,
                "small interior",
            ),
            (
                lat0 - 0.5,
                lat0 + 0.5,
                lon0 - 0.8,
                lon0 + 0.8,
                true,
                "wide interior",
            ),
            (
                lat0 - 0.03,
                lat0 + 0.03,
                lon0 - 0.9,
                lon0 + 0.9,
                true,
                "a thin strip",
            ),
            (
                lat0 - 3.0,
                lat0 + 0.3,
                lon0 - 3.0,
                lon0 + 0.3,
                false,
                "over the SW corner",
            ),
            (0.0, 90.0, -180.0, 0.0, false, "a quarter of the planet"),
            (
                lat0 + 8.0,
                lat0 + 9.0,
                lon0,
                lon0 + 1.0,
                false,
                "well off the grid",
            ),
        ];

        for &(min_lat, max_lat, min_lon, max_lon, tight, what) in boxes {
            let (fi0, fi1, fj0, fj1) = g
                .index_bounds(min_lat, max_lat, min_lon, max_lon)
                .unwrap_or_else(|| panic!("{what}: no bounds"));

            let (mut ti0, mut ti1) = (usize::MAX, 0usize);
            let (mut tj0, mut tj1) = (usize::MAX, 0usize);
            let mut inside = 0;
            for j in 0..template.nj as usize {
                for i in 0..template.ni as usize {
                    let (lat, lon) = g.latlon(i, j);
                    if lat < min_lat || lat > max_lat || lon < min_lon || lon > max_lon {
                        continue;
                    }
                    inside += 1;
                    assert!(
                        (fi0..=fi1).contains(&(i as f64)) && (fj0..=fj1).contains(&(j as f64)),
                        "{what}: point ({i}, {j}) is inside the box but outside \
                         i {fi0:.3}..{fi1:.3}, j {fj0:.3}..{fj1:.3}",
                    );
                    ti0 = ti0.min(i);
                    ti1 = ti1.max(i);
                    tj0 = tj0.min(j);
                    tj1 = tj1.max(j);
                }
            }

            if !tight {
                continue;
            }
            assert!(
                inside > 0,
                "{what}: an empty box proves nothing about tightness"
            );
            // Two cells of slack: the bounds are over the continuous box, the
            // true extent over lattice points, and a thin box's corner need not
            // contain one. Nowhere near enough slack to admit a degenerate
            // `0..ni`, which is tens of cells out on these boxes.
            for (got, want, axis) in [
                (fi0, ti0 as f64, "i lower"),
                (fi1, ti1 as f64, "i upper"),
                (fj0, tj0 as f64, "j lower"),
                (fj1, tj1 as f64, "j upper"),
            ] {
                assert!(
                    (got - want).abs() <= 2.0,
                    "{what}: {axis} bound {got:.3} is {:.3} cells off the {want} \
                     the {inside} contained points actually reach",
                    (got - want).abs(),
                );
            }
        }
    }

    /// Off the grid must be `None`, not a clamped edge point: the tooltip would
    /// otherwise report a CONUS reading for a cursor over the Atlantic.
    #[test]
    fn nearest_refuses_a_point_outside_the_grid() {
        let geometry = LambertGrid::from_template(&hrrr_conus_grid()).unwrap();
        for &(lat, lon, what) in &[
            (51.5, -0.13, "London"),
            (-33.87, 151.21, "Sydney"),
            (21.14, -140.0, "west of the SW corner"),
            (60.0, -97.5, "north of the domain"),
            (10.0, -97.5, "south of the domain"),
        ] {
            assert_eq!(geometry.nearest(lat, lon), None, "{what}");
        }
    }

    /// A projection can agree with PROJ at sampled points and still be indexed
    /// wrongly (transposed axes, sign flip on Dy). Scanning mode 0b0100_0000
    /// scans +i west-to-east and +j south-to-north.
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
        // NOAA's published HRRR CONUS domain: ~21.1N-52.6N, 134.1W-60.9W.
        // Loose on purpose; the tight numbers are the PROJ test's job.
        assert!(
            (21.0..22.0).contains(&min_lat) && (52.0..53.0).contains(&max_lat),
            "latitude span {min_lat}..{max_lat} is not the HRRR CONUS domain",
        );
        assert!(
            (-135.0..-133.0).contains(&min_lon) && (-61.5..-60.0).contains(&max_lon),
            "longitude span {min_lon}..{max_lon} is not the HRRR CONUS domain",
        );
    }

    /// Independent of every reference value above: asserts only that `forward`
    /// and `inverse` are mutual inverses, which the PROJ test cannot see.
    #[test]
    fn forward_of_inverse_returns_the_original_grid_index() {
        let grid = hrrr_conus_grid();
        let projection =
            LambertConformalConic::new(6_371_229.0, 6_371_229.0, 38.5, 262.5, 38.5, 38.5).unwrap();
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

    /// **Hand-worked invariant.** At `phi = lat_0`, `rho = rho0`; at
    /// `lambda = lon_0`, `theta = 0`; so eq. 14-1/14-2 give `x = y = 0` and
    /// `inverse(0, 0) == (lat_0, lon_0)` exactly for any valid parameter set.
    /// A self-consistent but individually wrong cone constant, scale factor or
    /// origin radius survives a single-point check but not this across all four.
    #[test]
    fn projection_origin_inverts_to_lat0_lon0() {
        let cases: &[(f64, f64, f64, f64, f64, f64, &str)] = &[
            // (a, b, lat_0, lon_0, lat_1, lat_2, label)
            (
                6_371_229.0,
                6_371_229.0,
                38.5,
                262.5,
                38.5,
                38.5,
                "HRRR (tangent, sphere)",
            ),
            (
                6_371_229.0,
                6_371_229.0,
                38.5,
                -97.5,
                30.0,
                60.0,
                "secant, sphere",
            ),
            (
                6_378_206.4,
                6_356_583.8,
                27.833333,
                -99.0,
                28.383333,
                30.283333,
                "secant, ellipsoid",
            ),
            (
                6_371_229.0,
                6_371_229.0,
                -38.5,
                145.0,
                -30.0,
                -60.0,
                "southern cone",
            ),
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

    /// **Hand-worked invariant.** `n = sin(38.5°) = 0.6225146366376195`, from
    /// `f64::sin` rather than from any projection code. The tangent branch is the
    /// only one HRRR uses and the one whose formula is not the textbook one —
    /// Snyder eq. 15-4 is 0/0 at `lat_1 == lat_2` and gives NaN.
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
        assert!(
            tangent.n.is_finite(),
            "tangent branch produced {}",
            tangent.n
        );
    }

    /// The tangent cone constant must come from **Latin1, not LaD**.
    ///
    /// Every other fixture here is HRRR-shaped, where `LaD == Latin1 == Latin2
    /// == 38.5°`, so `sin(LaD)` and `sin(Latin1)` are indistinguishable and that
    /// mutation survives the whole suite. Template 3.30 does not require them to
    /// agree, so this separates them: LaD = 45°, Latin1 = Latin2 = 30°.
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
    /// (rho0 is built from the same `n`), hence the other three rows.
    #[test]
    fn tangent_cone_constant_follows_latin1_not_lad() {
        let p =
            LambertConformalConic::new(6_371_229.0, 6_371_229.0, 45.0, -97.5, 30.0, 30.0).unwrap();

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

    /// The two branches must be one continuous function. Expected values come
    /// from `f64::sin` of the mean parallel, not from the projection.
    #[test]
    fn secant_cone_constant_approaches_the_tangent_limit() {
        for delta in [1.0_f64, 0.1, 0.01, 0.001] {
            let (lat1, lat2) = (38.5 - delta / 2.0, 38.5 + delta / 2.0);
            let secant =
                LambertConformalConic::new(6_371_229.0, 6_371_229.0, 38.5, 262.5, lat1, lat2)
                    .unwrap();
            // The secant cone constant tends to sin(38.5°) as δ -> 0 with error
            // O(δ²), so the bound is δ²-scaled.
            let expected = 38.5_f64.to_radians().sin();
            let bound = 1e-3 * delta * delta;
            assert!(
                (secant.n - expected).abs() < bound,
                "secant n for parallels ±{}° = {}, tangent limit {expected}, \
                 difference exceeds the O(δ²) bound {bound}",
                delta / 2.0,
                secant.n,
            );
            // And it must genuinely take the secant branch.
            assert_ne!(
                secant.n, expected,
                "delta={delta} was absorbed by the tangent epsilon",
            );
        }
    }

    /// Swapping the standard parallels is the one obvious mutation here that is
    /// *not* a bug: eq. 15-4 is antisymmetric in numerator and denominator, and
    /// eq. 15-2 gives the same `F` from either parallel by construction. No test
    /// suite can distinguish the orderings. PROJ 9.8.1 agrees — the EPSG example
    /// below differs by 3 nm between the two orders.
    #[test]
    fn exchanging_the_standard_parallels_is_a_no_op() {
        let a = 6_378_206.400;
        let b = a * (1.0 - 1.0 / 294.97870);
        let (lat0, lon0) = (27.0 + 50.0 / 60.0, -99.0);
        let (lat1, lat2) = (28.0 + 23.0 / 60.0, 30.0 + 17.0 / 60.0);

        let normal = LambertConformalConic::new(a, b, lat0, lon0, lat1, lat2).unwrap();
        let swapped = LambertConformalConic::new(a, b, lat0, lon0, lat2, lat1).unwrap();

        assert!(
            (normal.n - swapped.n).abs() < 1e-15,
            "cone constant differs"
        );
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
    /// The secant *and* ellipsoidal path, which no real input exercises: HRRR is
    /// tangent on a sphere, and grib's only upstream Lambert fixture
    /// (`ds.critfireo.bin`) is also tangent, at Latin1 = Latin2 = 25°. Without
    /// this the secant and ellipsoid branches ship unverified.
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
    /// This module has no false easting/northing (nor does grib's PROJ string),
    /// so the offsets are applied in the test. 1 US survey foot = 1200/3937 m.
    ///
    /// PROJ 9.8.1 gives (293676.579980, 77650.942539) m against the published
    /// (293676.5791, 77650.9423) m — sub-millimetre, so (B) and (C) corroborate.
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

    /// Without this, the ellipsoidal machinery could be dead code and the EPSG
    /// test would still pass. Measured separation at the EPSG test point is
    /// ~332 m in northing; the 100 m bound is a floor, not a fitted value.
    #[test]
    fn dropping_the_eccentricity_would_fail_the_epsg_example() {
        const US_SURVEY_FOOT_M: f64 = 1200.0 / 3937.0;
        let a = 6_378_206.400;
        let b = a * (1.0 - 1.0 / 294.97870);
        let (lat0, lon0, lat1, lat2) = (
            27.0 + 50.0 / 60.0,
            -99.0,
            28.0 + 23.0 / 60.0,
            30.0 + 17.0 / 60.0,
        );

        let ellipsoid = LambertConformalConic::new(a, b, lat0, lon0, lat1, lat2).unwrap();
        let sphere = LambertConformalConic::new(a, a, lat0, lon0, lat1, lat2).unwrap();

        assert!(
            ellipsoid.e > 0.0,
            "Clarke 1866 must have non-zero eccentricity"
        );
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

        let sphere_northing_usft = sy / US_SURVEY_FOOT_M;
        assert!(
            (sphere_northing_usft - 254_759.80).abs() > 100.0,
            "a spherical projection would still pass the EPSG check \
             (northing {sphere_northing_usft:.2} usft vs published 254759.80), \
             so that test proves nothing about the ellipsoidal path",
        );

        // The iteration must converge, not merely run.
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
        let p = LambertConformalConic::new(6_371_229.0, 6_371_229.0, -38.5, 145.0, -30.0, -60.0)
            .unwrap();
        assert!(
            p.n < 0.0,
            "southern parallels must give a negative cone constant"
        );

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
        assert!(
            latlons(&grid).is_err(),
            "unknown Code Table 3.2 value must error"
        );
    }

    /// Deliberately not a bit-for-bit clone of grib's `normalize_latlon`: grib's
    /// `(lon + 540) % 360 - 180` keeps the sign of the dividend and returns
    /// out-of-range values below -540° (-600° -> -240°), where `rem_euclid`
    /// gives the correct +120°. The two agree above -540°, which covers every
    /// longitude a GRIB2 grid can encode, so the divergence is unreachable here.
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
